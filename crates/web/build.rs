use std::path::PathBuf;
fn main() {
    println!("cargo:rerun-if-changed=static/app.js");
    println!("cargo:rerun-if-changed=static/style.css");
    println!("cargo:rerun-if-changed=static/index.template.html");
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let base = PathBuf::from(&manifest_dir);
    let template = std::fs::read_to_string(base.join("static/index.template.html")).expect("template");
    let js = std::fs::read_to_string(base.join("static/app.js")).expect("app.js");
    let css = std::fs::read_to_string(base.join("static/style.css")).expect("style.css");
    let output = template
        .replace("<style id=\"injected-css\"></style>", &format!("<style>\n{}\n</style>", css))
        .replace("<script id=\"injected-js\"></script>", &format!("<script>\n{}\n</script>", js));
    std::fs::write(base.join("static/index.html"), output).expect("write index.html");
}
