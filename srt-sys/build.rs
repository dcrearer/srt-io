use std::env;
use std::path::PathBuf;

fn main() {
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());

    // --- 1. Build vendored mbedTLS (static) ---------------------------------
    let mbedtls = cmake::Config::new("vendor/mbedtls")
        .define("ENABLE_TESTING", "OFF")
        .define("ENABLE_PROGRAMS", "OFF")
        .define("USE_SHARED_MBEDTLS_LIBRARY", "OFF")
        .define("USE_STATIC_MBEDTLS_LIBRARY", "ON")
        // cmake 4.x rejects the old cmake_minimum_required the vendored trees declare.
        .define("CMAKE_POLICY_VERSION_MINIMUM", "3.5")
        .build();

    // --- 2. Build vendored libsrt (static) against that mbedTLS --------------
    let srt = cmake::Config::new("vendor/libsrt")
        .define("ENABLE_APPS", "OFF")
        .define("ENABLE_SHARED", "OFF")
        .define("ENABLE_STATIC", "ON")
        .define("ENABLE_ENCRYPTION", "ON")
        .define("USE_ENCLIB", "mbedtls")
        .define("ENABLE_UNITTESTS", "OFF")
        .define("ENABLE_LOGGING", "ON")
        .define("CMAKE_POLICY_VERSION_MINIMUM", "3.5")
        // Let libsrt's FindMbedTLS locate our freshly-built static libs.
        .define("CMAKE_PREFIX_PATH", mbedtls.display().to_string())
        .define(
            "MBEDTLS_INCLUDE_DIR",
            format!("{}/include", mbedtls.display()),
        )
        .define("MBEDTLS_LIBRARY_DIR", format!("{}/lib", mbedtls.display()))
        .build();

    // --- 3. Link search paths (handle lib and lib64) ------------------------
    for dst in [&srt, &mbedtls] {
        println!("cargo:rustc-link-search=native={}/lib", dst.display());
        println!("cargo:rustc-link-search=native={}/lib64", dst.display());
    }

    // --- 4. Link static libs ------------------------------------------------
    println!("cargo:rustc-link-lib=static=srt");
    println!("cargo:rustc-link-lib=static=mbedtls");
    println!("cargo:rustc-link-lib=static=mbedx509");
    println!("cargo:rustc-link-lib=static=mbedcrypto");

    // libsrt is C++; pull in the C++ runtime.
    if cfg!(target_os = "macos") {
        println!("cargo:rustc-link-lib=c++");
    } else {
        println!("cargo:rustc-link-lib=stdc++");
    }

    // --- 5. Generate bindings ------------------------------------------------
    let include = format!("{}/include", srt.display());
    let bindings = bindgen::Builder::default()
        .header("wrapper.h")
        .clang_arg(format!("-I{include}"))
        .allowlist_function("srt_.*")
        .allowlist_type("SRT.*")
        .allowlist_type("srt_.*")
        .allowlist_var("SRT.*")
        .allowlist_var("srt_.*")
        .default_enum_style(bindgen::EnumVariation::Rust {
            non_exhaustive: false,
        })
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .expect("failed to generate libsrt bindings");
    bindings
        .write_to_file(out.join("bindings.rs"))
        .expect("failed to write bindings.rs");

    println!("cargo:rerun-if-changed=wrapper.h");
    println!("cargo:include={include}");
}
