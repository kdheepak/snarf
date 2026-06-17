use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=RUSTC");
    println!("cargo:rerun-if-env-changed=SNARF_BUILD_COMMIT");
    println!("cargo:rerun-if-env-changed=SNARF_BUILD_DATE");

    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let version = Command::new(rustc)
        .arg("--version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|version| version.trim().to_string())
        .filter(|version| !version.is_empty())
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=RUSTC_VERSION={version}");
    println!(
        "cargo:rustc-env=SNARF_BUILD_COMMIT={}",
        env_value("SNARF_BUILD_COMMIT")
    );
    println!(
        "cargo:rustc-env=SNARF_BUILD_DATE={}",
        env_value("SNARF_BUILD_DATE")
    );
}

fn env_value(name: &str) -> String {
    std::env::var(name).unwrap_or_default().trim().to_string()
}
