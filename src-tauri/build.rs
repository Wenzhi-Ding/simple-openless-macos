#[path = "src/build_target.rs"]
mod build_target;

fn main() {
    // build.rs 的 `#[cfg(target_os)]` 判断的是构建脚本主机，不是 Cargo 的目标平台。
    // 优先使用 Cargo 的目标 OS；旧工具链缺失该变量时回退解析 TARGET，避免 Linux
    let target = std::env::var("TARGET").unwrap_or_default();
    let target_os = build_target::classify_target_os(
        &target,
        std::env::var("CARGO_CFG_TARGET_OS").ok().as_deref(),
    );
    println!("cargo:warning=OpenLess build target={target}, target_os={target_os}");
    if target_os == "macos" {
        build_qwen_asr();
    }

    if target_os == "macos" {
        link_macos_compiler_runtime();
        let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
        if target_arch == "aarch64" {
            compile_mlx_metallib_path_shim();
        }
    }

    tauri_build::build();
}

/// MLX uses `__builtin_available` for newer Metal APIs. Rust links with
/// `-nodefaultlibs`, so the availability helper from Apple compiler-rt must be
/// added explicitly or Apple Silicon release links fail on macOS 14 targets.
fn link_macos_compiler_runtime() {
    let compiler = cc::Build::new().get_compiler();
    let output = std::process::Command::new(compiler.path())
        .arg("-print-resource-dir")
        .output()
        .expect("failed to query the macOS compiler resource directory");
    if !output.status.success() {
        panic!(
            "macOS compiler did not return its resource directory (status {})",
            output.status
        );
    }

    let resource_dir = String::from_utf8(output.stdout)
        .expect("macOS compiler resource directory was not UTF-8")
        .trim()
        .to_owned();
    let runtime_dir = std::path::PathBuf::from(resource_dir)
        .join("lib")
        .join("darwin");
    if !runtime_dir.join("libclang_rt.osx.a").exists() {
        panic!(
            "macOS compiler runtime not found at {}",
            runtime_dir.display()
        );
    }

    println!("cargo:rustc-link-search=native={}", runtime_dir.display());
    println!("cargo:rustc-link-lib=static=clang_rt.osx");
}

/// Apple Silicon 发布包把 mlx.metallib 放在 Contents/Resources。
/// mlx-c 默认只在可执行文件旁边找，这里补一个 C 入口去调用 set_metallib_path。
fn compile_mlx_metallib_path_shim() {
    const SOURCE: &str = "src/asr/local/mlx_set_metallib_path.cpp";
    println!("cargo:rerun-if-changed={SOURCE}");
    cc::Build::new()
        .cpp(true)
        .std("c++17")
        .file(SOURCE)
        .compile("openless_mlx_set_metallib_path");
}

/// 编译 vendored Open-Less/qwen-asr 的 C 源（macOS/Linux）。
///
/// 上游 Makefile `make blas` 等价配置：BLAS 加速通过 Accelerate framework，
/// `USE_BLAS` + `ACCELERATE_NEW_LAPACK` 是必要宏。
/// `-march=native` 这里**不**用——分发二进制要可移植，cc crate 在 release 下
/// 默认带 `-O2`，加上 `-O3` 提一档；NEON/AVX 在源码里有 `#ifdef` 自动分派。
fn build_qwen_asr() {
    const VENDOR: &str = "vendor/qwen-asr";
    const SOURCES: &[&str] = &[
        "qwen_asr.c",
        "qwen_asr_kernels.c",
        "qwen_asr_kernels_generic.c",
        "qwen_asr_kernels_neon.c",
        "qwen_asr_kernels_avx.c",
        "qwen_asr_audio.c",
        "qwen_asr_encoder.c",
        "qwen_asr_decoder.c",
        "qwen_asr_tokenizer.c",
        "qwen_asr_safetensors.c",
    ];

    let mut build = cc::Build::new();
    build
        .include(VENDOR)
        .flag("-O3")
        .flag("-ffast-math")
        // 上游开 `-Wall -Wextra`；我们把 qwen-asr 的代码当三方依赖，把无关警告压成静默
        // 避免 build log 噪音淹没我们自己的告警。
        .flag("-Wno-unused-parameter")
        .flag("-Wno-unused-variable")
        .flag("-Wno-unused-function")
        .flag("-Wno-sign-compare")
        .warnings(false);

    build
        .define("USE_BLAS", None)
        .define("ACCELERATE_NEW_LAPACK", None);

    for src in SOURCES {
        let path = format!("{}/{}", VENDOR, src);
        println!("cargo:rerun-if-changed={}", path);
        build.file(path);
    }
    println!("cargo:rerun-if-changed={}/qwen_asr.h", VENDOR);

    build.compile("qwen_asr");

    // BLAS = Accelerate
    println!("cargo:rustc-link-lib=framework=Accelerate");
}
