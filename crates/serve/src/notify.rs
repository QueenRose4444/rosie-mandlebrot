//! Messages when a job ends: a Discord webhook, and an upload of the finished
//! video to a URL of your own (multipart, field `file`, optional `X-Api-Key`).

use crate::jobs::{Job, JobState};
use crate::store::Notify;
use std::io::Read;
use std::path::Path;

/// Discord's attachment limit for webhooks without a boosted server, with a margin.
const DISCORD_FILE_LIMIT: u64 = 9_500_000;

/// The one-line summary of a finished job.
pub fn summary(job: &Job) -> String {
    let took = match (job.started_at, job.finished_at) {
        (Some(a), Some(b)) => format!(" in {}", human_secs(b - a)),
        _ => String::new(),
    };
    let what = match job.kind {
        "render" => "render",
        "assemble" => "new video",
        other => other,
    };
    match job.state {
        JobState::Done => format!("Rosie_Mandlebrot: {} finished — {what}{took}.", job.label),
        JobState::Stopped => format!("Rosie_Mandlebrot: {} stopped — {what}{took}; the video covers what was rendered.", job.label),
        JobState::Failed => format!(
            "Rosie_Mandlebrot: {} failed — {what}{took}: {}",
            job.label,
            job.error.clone().unwrap_or_else(|| "see the log".into())
        ),
        _ => format!("Rosie_Mandlebrot: {}", job.label),
    }
}

fn human_secs(s: i64) -> String {
    match s {
        s if s < 90 => format!("{s} s"),
        s if s < 5400 => format!("{} min", (s + 30) / 60),
        s => format!("{:.1} h", s as f64 / 3600.0),
    }
}

/// Send what the settings ask for. Blocking; run it off the async runtime.
/// Returns one line per destination for the log.
pub fn send(n: &Notify, job: &Job, video: Option<&Path>) -> Vec<String> {
    let mut report = Vec::new();
    if !n.discord_webhook.is_empty() {
        let attach = video.filter(|v| std::fs::metadata(v).is_ok_and(|m| m.len() <= DISCORD_FILE_LIMIT));
        report.push(match discord(&n.discord_webhook, &summary(job), attach) {
            Ok(()) => format!("Discord: sent{}", if attach.is_some() { " with the video" } else { "" }),
            Err(e) => format!("Discord: failed: {e:#}"),
        });
    }
    if let (false, Some(v)) = (n.upload_url.is_empty(), video) {
        report.push(match upload(&n.upload_url, &n.upload_api_key, v) {
            Ok(code) => format!("upload: {} answered {code}", n.upload_url),
            Err(e) => format!("upload: failed: {e:#}"),
        });
    }
    report
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(std::time::Duration::from_secs(600))).build().into()
}

/// A multipart/form-data body: text fields, then at most one file, streamed.
struct Multipart {
    boundary: String,
    head: Vec<u8>,
    file: Option<std::fs::File>,
    tail: Vec<u8>,
}

impl Multipart {
    fn new(fields: &[(&str, &str)], file: Option<(&str, &Path)>) -> std::io::Result<Multipart> {
        let boundary = format!("rosie-mandlebrot-{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
        let mut head = Vec::new();
        for (k, v) in fields {
            head.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n").as_bytes());
        }
        let file = match file {
            Some((field, path)) => {
                let name = path.file_name().map_or("video".into(), |n| n.to_string_lossy().replace('"', ""));
                let mime = match path.extension().and_then(|e| e.to_str()) {
                    Some("mp4") => "video/mp4",
                    Some("webm") => "video/webm",
                    Some("mov") => "video/quicktime",
                    Some("mkv") => "video/x-matroska",
                    _ => "application/octet-stream",
                };
                head.extend_from_slice(
                    format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{field}\"; filename=\"{name}\"\r\nContent-Type: {mime}\r\n\r\n").as_bytes(),
                );
                Some(std::fs::File::open(path)?)
            }
            None => None,
        };
        let tail = format!("{}--{boundary}--\r\n", if file.is_some() { "\r\n" } else { "" }).into_bytes();
        Ok(Multipart { boundary, head, file, tail })
    }

    /// The body's length, so it is not sent chunked (not every server takes that).
    fn len(&self) -> u64 {
        let file = self.file.as_ref().and_then(|f| f.metadata().ok()).map_or(0, |m| m.len());
        self.head.len() as u64 + file + self.tail.len() as u64
    }

    fn content_type(&self) -> String {
        format!("multipart/form-data; boundary={}", self.boundary)
    }

    fn reader(self) -> impl Read {
        let file: Box<dyn Read> = match self.file {
            Some(f) => Box::new(f),
            None => Box::new(std::io::empty()),
        };
        std::io::Cursor::new(self.head).chain(file).chain(std::io::Cursor::new(self.tail))
    }
}

fn discord(url: &str, text: &str, video: Option<&Path>) -> anyhow::Result<()> {
    let payload = serde_json::json!({ "content": text, "allowed_mentions": { "parse": [] } }).to_string();
    let mp = Multipart::new(&[("payload_json", &payload)], video.map(|v| ("files[0]", v)))?;
    let (ct, len) = (mp.content_type(), mp.len());
    agent().post(url).header("Content-Type", &ct).header("Content-Length", len.to_string()).send(ureq::SendBody::from_owned_reader(mp.reader()))?;
    Ok(())
}

fn upload(url: &str, api_key: &str, video: &Path) -> anyhow::Result<u16> {
    let name = video.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
    let mp = Multipart::new(&[("filename", &name), ("caller", "rosie_mandlebrot")], Some(("file", video)))?;
    let (ct, len) = (mp.content_type(), mp.len());
    let mut req = agent().post(url).header("Content-Type", &ct).header("Content-Length", len.to_string());
    if !api_key.is_empty() {
        req = req.header("X-Api-Key", api_key);
    }
    let resp = req.send(ureq::SendBody::from_owned_reader(mp.reader()))?;
    Ok(resp.status().as_u16())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};

    #[test]
    fn a_multipart_body_carries_the_fields_and_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("zoom.mp4");
        std::fs::write(&f, b"VIDEO").unwrap();
        let mp = Multipart::new(&[("caller", "x")], Some(("file", &f))).unwrap();
        let b = mp.boundary.clone();
        let mut body = String::new();
        mp.reader().read_to_string(&mut body).unwrap();
        assert!(body.starts_with(&format!("--{b}\r\nContent-Disposition: form-data; name=\"caller\"\r\n\r\nx\r\n")));
        assert!(body.contains("name=\"file\"; filename=\"zoom.mp4\"\r\nContent-Type: video/mp4\r\n\r\nVIDEO\r\n"));
        assert!(body.ends_with(&format!("--{b}--\r\n")));
    }

    /// A one-request HTTP server on 127.0.0.1: returns what it received.
    fn one_request_server() -> (String, std::thread::JoinHandle<String>) {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/up", l.local_addr().unwrap());
        let h = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut head = String::new();
            let mut len = 0usize;
            loop {
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
                head.push_str(&line);
                if line == "\r\n" {
                    break;
                }
            }
            let mut body = vec![0u8; len];
            r.read_exact(&mut body).unwrap();
            s.write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\r\n").unwrap();
            head + &String::from_utf8_lossy(&body)
        });
        (url, h)
    }

    #[test]
    fn the_upload_sends_the_file_and_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("zoom.mp4");
        std::fs::write(&f, b"VIDEO-BYTES").unwrap();
        let (url, h) = one_request_server();
        assert_eq!(upload(&url, "k123", &f).unwrap(), 202);
        let got = h.join().unwrap();
        assert!(got.to_ascii_lowercase().contains("x-api-key: k123"), "{got}");
        assert!(got.contains("VIDEO-BYTES"));
        assert!(got.contains("name=\"caller\"\r\n\r\nrosie_mandlebrot"));
    }
}
