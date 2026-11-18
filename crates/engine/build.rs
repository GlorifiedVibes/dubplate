fn main() {
    // The CUDA libtorch build ships libtorch.so → libtorch_cuda.so.
    // libtorch.so's global constructors register the CUDA hooks that make
    // tch::Cuda::is_available() return true.  However, the linker drops both
    // libraries under --as-needed (the Rust default) because no Rust symbol
    // directly references them — only tch's C glue does, via libtorch_cpu.so.
    //
    // Force-load libtorch.so (and transitively libtorch_cuda.so) so the hooks
    // are registered before main() runs.
    // Only the opt-in libtorch engine needs this; the default ONNX engine must not pull libtorch in.
    let torch = std::env::var_os("CARGO_FEATURE_TORCH_INFERENCE").is_some();
    if let (true, Ok(lib_dir)) = (torch, std::env::var("LIBTORCH")) {
        let lib = std::path::Path::new(&lib_dir).join("lib");
        let torch_so = lib.join("libtorch.so");
        let torch_cuda_so = lib.join("libtorch_cuda.so");
        if torch_so.exists() && torch_cuda_so.exists() {
            // The linker's default --as-needed drops libtorch.so and libtorch_cuda.so
            // because no Rust symbol directly references them — only tch's C glue does.
            // libtorch.so's global constructors register the CUDA hooks that make
            // tch::Cuda::is_available() return true, so we must force-load it.
            // --push-state/--pop-state scopes the --no-as-needed to just these two libs.
            println!("cargo:rustc-link-arg=-Wl,--push-state,--no-as-needed");
            println!("cargo:rustc-link-arg={}", torch_so.display());
            println!("cargo:rustc-link-arg={}", torch_cuda_so.display());
            println!("cargo:rustc-link-arg=-Wl,--pop-state");
        }
    }

    println!("cargo:rerun-if-env-changed=LIBTORCH");
}
