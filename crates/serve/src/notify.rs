//! Messages about jobs: a Discord webhook (a live message edited as a render
//! goes, a finished message that can ping, a message when uploads are done),
//! and an upload of the finished video (and, if asked, the run's pictures) to a
//! URL of your own (multipart, field `file`, optional `X-Api-Key`). Webhooks
//! can edit the messages they posted, so none of this needs a bot.

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

/// The mention a message starts with and the `allowed_mentions` that lets it
/// ping (and nothing else).
pub fn mention(ping: &str) -> (String, serde_json::Value) {
    match ping {
        "" => (String::new(), serde_json::json!({ "parse": [] })),
        "here" => ("@here ".into(), serde_json::json!({ "parse": ["everyone"] })),
        "everyone" => ("@everyone ".into(), serde_json::json!({ "parse": ["everyone"] })),
        role => (format!("<@&{role}> "), serde_json::json!({ "parse": [], "roles": [role] })),
    }
}

/// Send what the settings ask for. Blocking; run it off the async runtime.
/// `pictures` are uploaded after the video when the settings ask for them.
/// Returns one line per destination for the log.
pub fn send(n: &Notify, job: &Job, video: Option<&Path>, pictures: &[std::path::PathBuf]) -> Vec<String> {
    let mut report = Vec::new();
    if !n.discord_webhook.is_empty() {
        let attach = video.filter(|v| std::fs::metadata(v).is_ok_and(|m| m.len() <= DISCORD_FILE_LIMIT));
        let (at, allowed) = mention(&n.ping);
        report.push(match discord(&n.discord_webhook, &format!("{at}{}", summary(job)), &allowed, attach) {
            Ok(()) => format!("Discord: sent{}", if attach.is_some() { " with the video" } else { "" }),
            Err(e) => format!("Discord: failed: {e:#}"),
        });
    }
    if let (false, Some(v)) = (n.upload_url.is_empty(), video) {
        let run = job.run_id.clone().unwrap_or_else(|| job.label.clone());
        let mut sent = Vec::new();
        let mut failed = 0;
        let files: Vec<&Path> = std::iter::once(v).chain(pictures.iter().filter(|_| n.upload_pictures).map(|p| p.as_path())).collect();
        for f in &files {
            match upload(&n.upload_url, &n.upload_api_key, &n.fields(&run), f) {
                Ok(code) if (200..300).contains(&code) => sent.push(*f),
                Ok(code) => {
                    failed += 1;
                    report.push(format!("upload: {} answered {code} for {}", n.upload_url, f.display()));
                }
                Err(e) => {
                    failed += 1;
                    report.push(format!("upload: failed for {}: {e:#}", f.display()));
                }
            }
        }
        report.push(format!("upload: {} of {} files sent to {}", sent.len(), files.len(), n.upload_url));
        // The second message: the uploads are done.
        if !n.discord_webhook.is_empty() && !sent.is_empty() {
            let pics = sent.len().saturating_sub(1);
            let text = format!(
                "Rosie_Mandlebrot: {} uploaded — the video{}{}{}.",
                job.label,
                if pics > 0 { format!(" and {pics} picture{}", if pics == 1 { "" } else { "s" }) } else { String::new() },
                if failed > 0 { format!(" ({failed} failed)") } else { String::new() },
                n.fields(&run).iter().find(|(k, _)| k.contains("path") || k.contains("folder")).map_or(String::new(), |(_, v)| format!(", in {v}")),
            );
            let (at, allowed) = mention(&n.ping);
            report.push(match discord(&n.discord_webhook, &format!("{at}{text}"), &allowed, None) {
                Ok(()) => "Discord: upload message sent".into(),
                Err(e) => format!("Discord: upload message failed: {e:#}"),
            });
        }
    }
    report
}

/// The live message's text: what the job is doing and how far it has got.
pub fn progress_text(job: &Job) -> String {
    let p = &job.progress;
    let mut parts = Vec::new();
    if let Some(f) = p.fraction {
        parts.push(format!("{:.0}%", f * 100.0));
    }
    if let Some(s) = p.span {
        parts.push(format!("{s:.1e} deep"));
    }
    if let Some(n) = p.frames {
        parts.push(match p.frames_total {
            Some(t) => format!("frame {n} of {t}"),
            None => format!("{n} frames"),
        });
    }
    if let Some(e) = p.eta_secs {
        parts.push(format!("about {} left", human_secs(e.round() as i64)));
    }
    let doing = match (job.kind, job.stage.as_deref()) {
        (_, Some("assemble")) => "making the video",
        ("render", _) => "rendering",
        _ => "working",
    };
    let parts = if parts.is_empty() { String::new() } else { format!(" — {}", parts.join(", ")) };
    format!("Rosie_Mandlebrot: {doing} {}{parts}", job.label)
}

/// Post a message and return its id (`?wait=true` makes Discord answer with it).
pub fn post_message(webhook: &str, text: &str) -> anyhow::Result<String> {
    let url = with_query(webhook, "wait=true");
    let body = serde_json::json!({ "content": text, "allowed_mentions": { "parse": [] } });
    let v: serde_json::Value = serde_json::from_slice(&send_json("POST", &url, &body)?)?;
    v["id"].as_str().map(str::to_string).ok_or_else(|| anyhow::anyhow!("Discord did not say which message it posted"))
}

/// Replace a message this webhook posted.
pub fn edit_message(webhook: &str, id: &str, text: &str) -> anyhow::Result<()> {
    let (base, query) = webhook.split_once('?').map_or((webhook, None), |(b, q)| (b, Some(q)));
    let url = format!("{}/messages/{id}{}", base.trim_end_matches('/'), query.map_or(String::new(), |q| format!("?{q}")));
    send_json("PATCH", &url, &serde_json::json!({ "content": text, "allowed_mentions": { "parse": [] } }))?;
    Ok(())
}

fn with_query(url: &str, q: &str) -> String {
    format!("{url}{}{q}", if url.contains('?') { "&" } else { "?" })
}

/// A JSON request, IPv4 first and then any address (some networks offer IPv6
/// that goes nowhere, which stalled each request for about a minute).
fn send_json(method: &str, url: &str, body: &serde_json::Value) -> anyhow::Result<Vec<u8>> {
    let go = |a: ureq::Agent| -> anyhow::Result<Vec<u8>> {
        let req = match method {
            "PATCH" => a.patch(url),
            _ => a.post(url),
        };
        let resp = req.header("Content-Type", "application/json").send(body.to_string())?;
        Ok(resp.into_body().with_config().limit(1 << 20).read_to_vec()?)
    };
    go(agent_for(ureq::config::IpFamily::Ipv4Only, 20)).or_else(|_| go(agent_for(ureq::config::IpFamily::Any, 60)))
}

fn agent_for(family: ureq::config::IpFamily, secs: u64) -> ureq::Agent {
    ureq::Agent::config_builder().ip_family(family).timeout_global(Some(std::time::Duration::from_secs(secs))).build().into()
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

fn discord(url: &str, text: &str, allowed: &serde_json::Value, video: Option<&Path>) -> anyhow::Result<()> {
    if video.is_none() {
        send_json("POST", url, &serde_json::json!({ "content": text, "allowed_mentions": allowed }))?;
        return Ok(());
    }
    let payload = serde_json::json!({ "content": text, "allowed_mentions": allowed }).to_string();
    let mp = Multipart::new(&[("payload_json", &payload)], video.map(|v| ("files[0]", v)))?;
    let (ct, len) = (mp.content_type(), mp.len());
    agent().post(url).header("Content-Type", &ct).header("Content-Length", len.to_string()).send(ureq::SendBody::from_owned_reader(mp.reader()))?;
    Ok(())
}

fn upload(url: &str, api_key: &str, extra: &[(String, String)], video: &Path) -> anyhow::Result<u16> {
    let name = video.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
    let mut fields: Vec<(&str, &str)> = vec![("filename", &name), ("caller", "rosie_mandlebrot")];
    fields.extend(extra.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    let mp = Multipart::new(&fields, Some(("file", video)))?;
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
    fn mentions_ping_only_whom_they_say() {
        assert_eq!(mention(""), (String::new(), serde_json::json!({ "parse": [] })));
        assert_eq!(mention("here").0, "@here ");
        assert_eq!(mention("everyone").1, serde_json::json!({ "parse": ["everyone"] }));
        assert_eq!(mention("1234567890"), ("<@&1234567890> ".into(), serde_json::json!({ "parse": [], "roles": ["1234567890"] })));
    }

    #[test]
    fn a_live_message_is_posted_then_edited_in_place() {
        // Post: ?wait=true, and the id comes back.
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/api/webhooks/1/tok", l.local_addr().unwrap());
        let h = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for _ in 0..2 {
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
                let reply = r#"{"id":"987654321"}"#;
                s.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{reply}", reply.len()).as_bytes()).unwrap();
                seen.push(head.lines().next().unwrap().to_string() + " " + &String::from_utf8_lossy(&body));
            }
            seen
        });
        let id = post_message(&base, "rendering seahorse — 0%").unwrap();
        assert_eq!(id, "987654321");
        edit_message(&base, &id, "rendering seahorse — 50%").unwrap();
        let seen = h.join().unwrap();
        assert!(seen[0].starts_with("POST /api/webhooks/1/tok?wait=true "), "{}", seen[0]);
        assert!(seen[0].contains("0%"));
        assert!(seen[1].starts_with("PATCH /api/webhooks/1/tok/messages/987654321 "), "{}", seen[1]);
        assert!(seen[1].contains("50%"));
    }

    #[test]
    fn the_upload_sends_the_file_and_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("zoom.mp4");
        std::fs::write(&f, b"VIDEO-BYTES").unwrap();
        let (url, h) = one_request_server();
        let extra = vec![("drive_path".to_string(), "20261026-seahorse".to_string())];
        assert_eq!(upload(&url, "k123", &extra, &f).unwrap(), 202);
        let got = h.join().unwrap();
        assert!(got.to_ascii_lowercase().contains("x-api-key: k123"), "{got}");
        assert!(got.contains("VIDEO-BYTES"));
        assert!(got.contains("name=\"caller\"\r\n\r\nrosie_mandlebrot"));
        assert!(got.contains("name=\"drive_path\"\r\n\r\n20261026-seahorse"), "the extra fields go with it");
    }
}
