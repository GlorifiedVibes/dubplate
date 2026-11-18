//! The graphics adapters of this PC, so vocal removal can use the one with the most memory.
//!
//! DirectML's own default is the first adapter Windows lists, and on a PC with a small integrated GPU (a
//! Radeon with 486 MB) that is the one that runs out of memory. On other systems the list is empty: CUDA
//! and CoreML choose for themselves.

#[derive(Clone, Debug, PartialEq)]
pub struct Adapter {
    /// DirectML's number for it (its place in Windows' list).
    pub index: i32,
    pub name: String,
    /// Memory of its own, in bytes (integrated chips share the PC's and report little).
    pub dedicated: u64,
}

/// Real graphics adapters (not Windows' software one), in Windows' order.
#[cfg(windows)]
pub fn adapters() -> Vec<Adapter> {
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE};
    let mut out = Vec::new();
    let Ok(factory) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else { return out };
    let mut i = 0u32;
    while let Ok(adapter) = unsafe { factory.EnumAdapters1(i) } {
        if let Ok(d) = unsafe { adapter.GetDesc1() } {
            if d.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 == 0 {
                let end = d.Description.iter().position(|&c| c == 0).unwrap_or(d.Description.len());
                out.push(Adapter {
                    index: i as i32,
                    name: String::from_utf16_lossy(&d.Description[..end]),
                    dedicated: d.DedicatedVideoMemory as u64,
                });
            }
        }
        i += 1;
    }
    out
}

#[cfg(not(windows))]
pub fn adapters() -> Vec<Adapter> {
    Vec::new()
}

/// The adapter with the most memory of its own, if there is more than one to choose from.
pub fn biggest(list: &[Adapter]) -> Option<&Adapter> {
    list.iter().max_by_key(|a| a.dedicated)
}

/// One line per adapter, for `--gpus` and the log.
pub fn describe(list: &[Adapter]) -> String {
    if list.is_empty() {
        return "no graphics adapters found".to_owned();
    }
    list.iter()
        .map(|a| format!("  {}: {} ({} MB)", a.index, a.name, a.dedicated / (1024 * 1024)))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_adapter_with_the_most_memory_is_chosen_not_the_first() {
        let list = vec![
            Adapter { index: 0, name: "AMD Radeon(TM) Graphics".into(), dedicated: 486 << 20 },
            Adapter { index: 1, name: "NVIDIA GeForce RTX".into(), dedicated: 12 << 30 },
        ];
        assert_eq!(biggest(&list).map(|a| a.index), Some(1));
        assert!(describe(&list).contains("1: NVIDIA GeForce RTX (12288 MB)"));
        assert_eq!(biggest(&[]), None);
    }
}
