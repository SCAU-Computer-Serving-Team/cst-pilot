use std::env;
use std::fs;
#[cfg(feature = "embed-web")]
use std::path::Path;
use std::path::PathBuf;

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let out_file = out_dir.join("embedded_web.rs");

    #[cfg(not(feature = "embed-web"))]
    {
        fs::write(
            &out_file,
            "pub const EMBEDDED_WEB_FILES: &[(&str, &[u8])] = &[];\n",
        )
        .expect("write empty embedded_web.rs");
    }

    #[cfg(feature = "embed-web")]
    {
        let manifest_dir =
            PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
        let web_dir = manifest_dir
            .join("../../src/web/frontend/build/client")
            .canonicalize()
            .expect(
                "embed-web requires built web frontend under src/web/frontend/build/client; run `npm run build --prefix src/web/frontend` first",
            );
        if !web_dir.join("index.html").is_file() {
            panic!(
                "embed-web requires {} to exist; run `npm run build --prefix src/web/frontend` first",
                web_dir.join("index.html").display()
            );
        }
        println!("cargo:rerun-if-changed={}", web_dir.display());
        let mut entries: Vec<(String, PathBuf)> = Vec::new();
        collect_files(&web_dir, &web_dir, &mut entries);
        entries.sort_by(|a, b| a.0.cmp(&b.0));

        let mut code = String::from("pub const EMBEDDED_WEB_FILES: &[(&str, &[u8])] = &[\n");
        for (rel, abs) in &entries {
            println!("cargo:rerun-if-changed={}", abs.display());
            let rel_lit = serde_escape(rel);
            let abs_lit = serde_escape(&abs.to_string_lossy());
            code.push_str(&format!("    ({rel_lit}, include_bytes!({abs_lit})),\n"));
        }
        code.push_str("];\n");
        fs::write(&out_file, code).expect("write embedded_web.rs");
    }
}

#[cfg(feature = "embed-web")]
fn collect_files(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let mut items: Vec<_> = fs::read_dir(dir)
        .expect("read web static dir")
        .filter_map(Result::ok)
        .collect();
    items.sort_by_key(|e| e.file_name());
    for entry in items {
        let path = entry.path();
        let rel = path
            .strip_prefix(root)
            .expect("strip prefix")
            .to_string_lossy()
            .replace('\\', "/");
        if rel == "fonts" || rel.starts_with("fonts/") {
            continue;
        }
        if path.is_dir() {
            collect_files(root, &path, out);
        } else if path.is_file() {
            out.push((rel, path));
        }
    }
}

#[cfg(feature = "embed-web")]
fn serde_escape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 2);
    out.push('"');
    for ch in raw.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
