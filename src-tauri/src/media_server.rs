//! Minimal loopback HTTP server for video files.
//!
//! WebKitGTK (Linux) can't stream `<video>` from Tauri's custom `asset://`
//! protocol, so videos are served from `http://127.0.0.1:<port>/<token>/name`
//! instead, which works with every WebView. Only files explicitly registered
//! by the app are reachable, each behind an unguessable random token, and
//! the socket listens on loopback only.

use std::collections::HashMap;
use std::collections::hash_map::RandomState;
use std::fs::File;
use std::hash::{BuildHasher, Hasher};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub struct MediaServer {
    port: u16,
    files: Mutex<HashMap<String, PathBuf>>,
}

impl MediaServer {
    pub fn start() -> std::io::Result<Arc<Self>> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let server = Arc::new(MediaServer {
            port: listener.local_addr()?.port(),
            files: Mutex::new(HashMap::new()),
        });
        let s = server.clone();
        std::thread::Builder::new()
            .name("media-server".into())
            .spawn(move || {
                for stream in listener.incoming().flatten() {
                    let s = s.clone();
                    std::thread::spawn(move || {
                        if let Err(e) = s.handle(stream) {
                            log::debug!("media server: {e}");
                        }
                    });
                }
            })?;
        log::info!("media server on 127.0.0.1:{}", server.port);
        Ok(server)
    }

    /// Returns a URL for `path`, registering it if needed.
    pub fn url_for(&self, path: &Path) -> String {
        let mut files = self.files.lock().unwrap();
        let token = match files.iter().find(|(_, p)| p.as_path() == path) {
            Some((t, _)) => t.clone(),
            None => {
                let t = random_token();
                files.insert(t.clone(), path.to_path_buf());
                t
            }
        };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        format!(
            "http://127.0.0.1:{}/{token}/{}",
            self.port,
            percent_encode(&name)
        )
    }

    fn handle(&self, stream: TcpStream) -> std::io::Result<()> {
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut out = stream;
        let mut request_line = String::new();
        reader.read_line(&mut request_line)?;
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or("");
        let target = parts.next().unwrap_or("");

        let mut range = None;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 || line == "\r\n" || line == "\n" {
                break;
            }
            if let Some((k, v)) = line.split_once(':')
                && k.trim().eq_ignore_ascii_case("range")
            {
                range = Some(v.trim().to_string());
            }
        }

        if method != "GET" && method != "HEAD" {
            return respond_status(&mut out, "405 Method Not Allowed");
        }
        let token = target
            .trim_start_matches('/')
            .split('/')
            .next()
            .unwrap_or("");
        let Some(path) = self.files.lock().unwrap().get(token).cloned() else {
            return respond_status(&mut out, "404 Not Found");
        };
        let Ok(mut file) = File::open(&path) else {
            return respond_status(&mut out, "404 Not Found");
        };
        let size = file.metadata()?.len();

        let (status, start, end) = match range.as_deref().and_then(|r| parse_range(r, size)) {
            Some((s, e)) => ("206 Partial Content", s, e),
            None if range.is_some() => {
                write!(
                    out,
                    "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{size}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )?;
                return Ok(());
            }
            None => ("200 OK", 0, size.saturating_sub(1)),
        };
        let len = if size == 0 { 0 } else { end - start + 1 };

        let mut head = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {}\r\nContent-Length: {len}\r\nAccept-Ranges: bytes\r\nAccess-Control-Allow-Origin: *\r\nCache-Control: no-store\r\nConnection: close\r\n",
            mime_for(&path)
        );
        if status.starts_with("206") {
            head.push_str(&format!("Content-Range: bytes {start}-{end}/{size}\r\n"));
        }
        head.push_str("\r\n");
        out.write_all(head.as_bytes())?;
        if method == "GET" && len > 0 {
            file.seek(SeekFrom::Start(start))?;
            // Broken pipes are normal: the player aborts requests when seeking.
            let _ = std::io::copy(&mut file.take(len), &mut out);
        }
        Ok(())
    }
}

fn respond_status(out: &mut TcpStream, status: &str) -> std::io::Result<()> {
    write!(
        out,
        "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
}

/// Parses a single `bytes=` range into an inclusive (start, end).
fn parse_range(header: &str, size: u64) -> Option<(u64, u64)> {
    let spec = header.strip_prefix("bytes=")?.split(',').next()?.trim();
    let (a, b) = spec.split_once('-')?;
    if size == 0 {
        return None;
    }
    let (start, end) = if a.is_empty() {
        let suffix: u64 = b.parse().ok()?;
        (size.saturating_sub(suffix), size - 1)
    } else {
        let start: u64 = a.parse().ok()?;
        let end = if b.is_empty() {
            size - 1
        } else {
            b.parse::<u64>().ok()?.min(size - 1)
        };
        (start, end)
    };
    (start <= end && start < size).then_some((start, end))
}

fn mime_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("mp4" | "m4v") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mkv") => "video/x-matroska",
        Some("mov") => "video/quicktime",
        Some("ogv") => "video/ogg",
        Some("avi") => "video/x-msvideo",
        _ => "application/octet-stream",
    }
}

fn random_token() -> String {
    // 128 bits from the OS-seeded SipHash keys plus time; good enough for an
    // unguessable loopback URL without pulling in a RNG crate.
    let mut out = String::new();
    for i in 0..2u64 {
        let mut h = RandomState::new().build_hasher();
        h.write_u64(i);
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
        );
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::parse_range;

    #[test]
    fn ranges() {
        assert_eq!(parse_range("bytes=0-", 100), Some((0, 99)));
        assert_eq!(parse_range("bytes=10-19", 100), Some((10, 19)));
        assert_eq!(parse_range("bytes=-10", 100), Some((90, 99)));
        assert_eq!(parse_range("bytes=50-500", 100), Some((50, 99)));
        assert_eq!(parse_range("bytes=100-", 100), None);
        assert_eq!(parse_range("items=0-1", 100), None);
    }
}
