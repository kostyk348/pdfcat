// Сборка ресурсов Windows (иконка) без внешних крейтов: windres -> .o -> линковка.
use std::env;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=assets/pdfcat.ico");
    println!("cargo:rerun-if-changed=assets/resource.rc");
    let out = env::var("OUT_DIR").unwrap_or_else(|_| "target".into()).replace('\\', "/");
    let obj = format!("{}/resource.o", out);
    // windres ищем в PATH (Mingw-w64) — при отсутствии собираем без иконки
    for windres in ["x86_64-w64-mingw32-windres", "windres"] {
        let ok = Command::new(windres)
            .args(["assets/resource.rc", "-O", "coff", "-o"])
            .arg(&obj)
            .status();
        if let Ok(st) = ok {
            if st.success() {
                println!("cargo:rustc-link-arg-bins={}", obj);
                return;
            }
        }
    }
    println!("cargo:warning=windres не найден — exe без иконки");
}
