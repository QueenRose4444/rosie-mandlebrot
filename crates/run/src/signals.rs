//! Reads commands from stdin on a background thread:
//!   stop / quit   -> flip the stop flag (graceful drain + video encode)
//!   pause         -> stop without making the video (the run is resumed later)
//!   status        -> flip the status flag (main prints current snapshot)
//!   flush         -> flip the flush flag (force checkpoint commit)
//!
//! Any unrecognized line is ignored.

use std::io::BufRead;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::thread::JoinHandle;

#[derive(Clone, Default)]
pub struct StdinSignals {
    pub stop: Arc<AtomicBool>,
    pub status_request: Arc<AtomicBool>,
    /// Set with `stop` by `pause`: end without the video step.
    pub pause: Arc<AtomicBool>,
}

pub fn spawn_listener(sig: StdinSignals) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("stdin-cmds".into())
        .spawn(move || {
            let stdin = std::io::stdin();
            let mut line = String::new();
            let mut lock = stdin.lock();
            loop {
                line.clear();
                match lock.read_line(&mut line) {
                    Ok(0) => break, // EOF
                    Ok(_) => {
                        let cmd = line.trim().to_ascii_lowercase();
                        match cmd.as_str() {
                            "stop" | "quit" | "exit" => {
                                tracing::info!("stop command received from stdin");
                                sig.stop.store(true, std::sync::atomic::Ordering::SeqCst);
                                break;
                            }
                            "pause" => {
                                tracing::info!("pause command received from stdin: stopping without making the video");
                                sig.pause.store(true, std::sync::atomic::Ordering::SeqCst);
                                sig.stop.store(true, std::sync::atomic::Ordering::SeqCst);
                                break;
                            }
                            "status" => {
                                sig.status_request
                                    .store(true, std::sync::atomic::Ordering::SeqCst);
                            }
                            "" => {}
                            other => {
                                tracing::debug!("ignoring stdin command: {other:?}");
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        })
        .expect("spawn stdin-cmds thread")
}
