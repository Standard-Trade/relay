//! Media hosting: copy each slide into the served folder as a JPEG named by
//! its content hash, and serve that folder over HTTP.
//!
//! Instagram only accepts JPEG, and TikTok pulls photos from a URL on a
//! domain verified in its developer portal, so both read from here.

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Convert (if needed) and copy an image into `media_dir`; return its file name.
pub fn publish_image(src: &Path, media_dir: &Path) -> Result<String> {
    let bytes = std::fs::read(src).with_context(|| format!("reading {}", src.display()))?;
    let hash = hex(&Sha256::digest(&bytes)[..12]);
    let name = format!("{hash}.jpg");
    let dest = media_dir.join(&name);
    if !dest.exists() {
        std::fs::create_dir_all(media_dir)?;
        let img = image::load_from_memory(&bytes).with_context(|| format!("decoding {}", src.display()))?;
        let rgb = img.to_rgb8();
        let tmp = media_dir.join(format!(".{name}.tmp"));
        let mut out = std::fs::File::create(&tmp)?;
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 92).encode_image(&rgb)?;
        std::fs::rename(&tmp, &dest)?;
    }
    Ok(name)
}

pub fn url(base: &str, name: &str) -> String {
    format!("{}/{}", base.trim_end_matches('/'), name)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Resolve a request path to a file inside `root`, refusing anything that
/// could escape it.
pub fn resolve(root: &Path, request_path: &str) -> Option<PathBuf> {
    let name = request_path.trim_start_matches('/');
    let ok = !name.is_empty()
        && name.len() <= 64
        && name.ends_with(".jpg")
        && name.chars().all(|c| c.is_ascii_hexdigit() || c == '.' || c == 'j' || c == 'p' || c == 'g')
        && !name.contains("..")
        && !name.starts_with('.');
    if !ok {
        return None;
    }
    let p = root.join(name);
    p.is_file().then_some(p)
}

/// Serve `root` read-only on `addr` (put a tunnel or proxy with your verified
/// domain in front of it).
pub fn serve(root: &Path, addr: &str) -> Result<()> {
    let server = tiny_http::Server::http(addr).map_err(|e| anyhow::anyhow!("binding {addr}: {e}"))?;
    eprintln!("serving {} on http://{addr}", root.display());
    for req in server.incoming_requests() {
        let path = req.url().split('?').next().unwrap_or("").to_string();
        let resp = match (req.method(), resolve(root, &path)) {
            (tiny_http::Method::Get | tiny_http::Method::Head, Some(file)) => {
                let f = std::fs::File::open(file)?;
                let header = tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"image/jpeg"[..]).unwrap();
                req.respond(tiny_http::Response::from_file(f).with_header(header))
            }
            _ => req.respond(tiny_http::Response::empty(404)),
        };
        if let Err(e) = resp {
            eprintln!("respond error: {e}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_refuses_escapes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("abc123.jpg"), b"x").unwrap();
        assert!(resolve(dir.path(), "/abc123.jpg").is_some());
        for bad in ["/../etc/passwd", "/abc123.png", "/", "/.abc.jpg", "/a/../abc123.jpg", "/abc%2f.jpg"] {
            assert!(resolve(dir.path(), bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn png_becomes_jpeg_once() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("s.png");
        image::RgbImage::from_pixel(8, 8, image::Rgb([200, 10, 10])).save(&src).unwrap();
        let media = dir.path().join("media");
        let a = publish_image(&src, &media).unwrap();
        let b = publish_image(&src, &media).unwrap();
        assert_eq!(a, b);
        let bytes = std::fs::read(media.join(&a)).unwrap();
        assert_eq!(&bytes[..2], &[0xFF, 0xD8]);
    }
}
