fn main() {
    // Rebuild when the (gitignored, build-time) issue-report DB URI changes so `option_env!` picks
    // up a newly-set value instead of a stale cached one.
    println!("cargo:rerun-if-env-changed=WISTERIA_REPORT_URI");
    tauri_build::build()
}
