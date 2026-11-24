// Сборка ресурсов Windows (иконка) без внешних крейтов: windres -> .o -> линковка.
use std::env;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=assets/pdfcat.ico");
    println!("cargo:rerun-if-changed=assets/resource.rc");
    let out = env::var("OUT_DIR").unwrap_or_else(|_| "target".into()).replace('\\', "/");
    let obj = format!("{}/resource.o", out);
    // Win7: свежий mingw ld ставит SubsystemVersion 8.0 (запуск только с Win8) —
    // для gnu-таргета явно понижаем до 6.0. Для msvc-таргета флаги несовместимы.
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_env == "gnu" {
        println!("cargo:rustc-link-arg-bins=-Wl,--major-subsystem-version,6");
        println!("cargo:rustc-link-arg-bins=-Wl,--minor-subsystem-version,0");
    }
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
