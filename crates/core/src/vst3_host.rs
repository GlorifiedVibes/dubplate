//! Hosts VST3 effect plugins: finds them, loads one, and runs blocks of stereo audio through it.
//!
//! Experimental. A plugin runs inside this process, so one that crashes takes the app with it; a safe version
//! needs the plugin in a process of its own (see docs/vst3.md). No plugin windows yet: parameters are listed
//! and set from the host.

use std::ffi::{c_char, c_void};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use vst3::Steinberg::Vst::{
    AudioBusBuffers, AudioBusBuffers__type0, BusDirections_, IAudioProcessor, IAudioProcessorTrait, IComponent,
    IComponentHandler, IComponentHandlerTrait, IComponentTrait, IConnectionPoint, IConnectionPointTrait,
    IEditController, IEditControllerTrait, IHostApplication, IHostApplicationTrait, IParamValueQueue,
    IParamValueQueueTrait, IParameterChanges, IParameterChangesTrait, MediaTypes_, ParameterInfo, ParameterInfo_,
    ProcessContext, ProcessContext_, ProcessData, ProcessModes_, ProcessSetup, SpeakerArr, SymbolicSampleSizes_,
};
use vst3::Steinberg::{
    kResultOk, FUnknown, IPluginBaseTrait, IPluginFactory, IPluginFactory2, IPluginFactory2Trait, IPluginFactoryTrait,
    PClassInfo, PClassInfo2, PFactoryInfo,
};
use vst3::{Class, ComPtr, ComWrapper, Interface};

/// A plugin found on disk.
#[derive(Debug, Clone)]
pub struct PluginInfo {
    pub name: String,
    pub vendor: String,
    pub bundle: PathBuf,
    /// An effect (has an audio input and says so), as opposed to an instrument.
    pub is_effect: bool,
    pub class_index: usize,
}

/// One automatable parameter.
#[derive(Debug, Clone)]
pub struct Param {
    pub id: u32,
    pub name: String,
    pub units: String,
    /// 0 for a continuous parameter, else the number of steps above the first.
    pub steps: i32,
    pub default: f64,
}

/// The folders VST3 plugins live in.
pub fn search_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if cfg!(target_os = "linux") {
        if let Some(h) = std::env::var_os("HOME") {
            v.push(Path::new(&h).join(".vst3"));
        }
        v.push("/usr/lib/vst3".into());
        v.push("/usr/local/lib/vst3".into());
    } else if cfg!(target_os = "windows") {
        if let Some(p) = std::env::var_os("CommonProgramFiles") {
            v.push(Path::new(&p).join("VST3"));
        }
    } else if cfg!(target_os = "macos") {
        v.push("/Library/Audio/Plug-Ins/VST3".into());
        if let Some(h) = std::env::var_os("HOME") {
            v.push(Path::new(&h).join("Library/Audio/Plug-Ins/VST3"));
        }
    }
    if let Some(extra) = std::env::var_os("DUBPLATE_VST3_PATH") {
        v.extend(std::env::split_paths(&extra));
    }
    v
}

/// Every `.vst3` bundle under `dirs`, looking up to four folders down (vendors keep them in sub-folders, and an
/// unpacked archive can leave them in `lib/vst3`). A bundle's own insides are not searched.
pub fn find_bundles(dirs: &[PathBuf]) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "vst3") {
                out.push(p);
            } else if depth > 0 && p.is_dir() {
                walk(&p, depth - 1, out);
            }
        }
    }
    let mut out = Vec::new();
    for d in dirs {
        walk(d, 4, &mut out);
    }
    out.sort();
    out.dedup();
    out
}

fn module_file(bundle: &Path) -> PathBuf {
    let stem = bundle.file_stem().and_then(|s| s.to_str()).unwrap_or("plugin");
    if cfg!(target_os = "linux") {
        bundle.join("Contents").join(format!("{}-linux", std::env::consts::ARCH)).join(format!("{stem}.so"))
    } else if cfg!(target_os = "windows") {
        bundle.join("Contents").join("x86_64-win").join(format!("{stem}.vst3"))
    } else {
        bundle.join("Contents").join("MacOS").join(stem)
    }
}

fn text<const N: usize>(a: &[c_char; N]) -> String {
    let b: Vec<u8> = a.iter().map(|&c| c as u8).take_while(|&c| c != 0).collect();
    String::from_utf8_lossy(&b).into_owned()
}

fn text16(a: &[u16]) -> String {
    String::from_utf16_lossy(&a.iter().copied().take_while(|&c| c != 0).collect::<Vec<_>>())
}

/// A loaded plugin file: keeps the library open, and calls the module's exit function on drop.
struct Module {
    lib: Option<libloading::Library>,
    factory: Option<ComPtr<IPluginFactory>>,
}

impl Module {
    fn open(bundle: &Path) -> Result<Module, String> {
        let file = module_file(bundle);
        if !file.is_file() {
            return Err(format!("no plugin file at {}", file.display()));
        }
        unsafe {
            #[cfg(unix)]
            let lib = {
                let l = libloading::os::unix::Library::new(&file)
                    .map_err(|e| format!("can't load {}: {e}", file.display()))?;
                // Linux and macOS plugins want the library handle in their entry call.
                let raw = l.into_raw();
                let l = libloading::os::unix::Library::from_raw(raw);
                if let Ok(entry) = l.get::<unsafe extern "C" fn(*mut c_void) -> bool>(b"ModuleEntry\0") {
                    if !entry(raw) {
                        return Err("the plugin refused to start".into());
                    }
                } else if let Ok(entry) = l.get::<unsafe extern "C" fn(*mut c_void) -> bool>(b"bundleEntry\0") {
                    if !entry(raw) {
                        return Err("the plugin refused to start".into());
                    }
                }
                libloading::Library::from(l)
            };
            #[cfg(not(unix))]
            let lib = {
                let l = libloading::Library::new(&file).map_err(|e| format!("can't load {}: {e}", file.display()))?;
                if let Ok(init) = l.get::<unsafe extern "C" fn() -> bool>(b"InitDll\0") {
                    if !init() {
                        return Err("the plugin refused to start".into());
                    }
                }
                l
            };
            let get: libloading::Symbol<unsafe extern "C" fn() -> *mut IPluginFactory> =
                lib.get(b"GetPluginFactory\0").map_err(|_| "not a VST3 plugin (no GetPluginFactory)".to_string())?;
            let factory = ComPtr::from_raw(get()).ok_or("the plugin gave no factory")?;
            Ok(Module { lib: Some(lib), factory: Some(factory) })
        }
    }

    fn factory(&self) -> &ComPtr<IPluginFactory> {
        self.factory.as_ref().unwrap()
    }
}

impl Drop for Module {
    fn drop(&mut self) {
        self.factory = None;
        if let Some(lib) = self.lib.take() {
            unsafe {
                #[cfg(unix)]
                for name in [&b"ModuleExit\0"[..], &b"bundleExit\0"[..]] {
                    if let Ok(exit) = lib.get::<unsafe extern "C" fn() -> bool>(name) {
                        exit();
                        break;
                    }
                }
                #[cfg(not(unix))]
                if let Ok(exit) = lib.get::<unsafe extern "C" fn() -> bool>(b"ExitDll\0") {
                    exit();
                }
            }
            drop(lib);
        }
    }
}

/// Lists the audio plugins in one bundle (a bundle can hold several).
pub fn inspect(bundle: &Path) -> Result<Vec<PluginInfo>, String> {
    let m = Module::open(bundle)?;
    let f = m.factory();
    let mut vendor = PFactoryInfo { vendor: [0; 64], url: [0; 256], email: [0; 128], flags: 0 };
    unsafe {
        f.getFactoryInfo(&mut vendor);
    }
    let f2 = f.cast::<IPluginFactory2>();
    let mut out = Vec::new();
    for i in 0..unsafe { f.countClasses() } {
        unsafe {
            let (name, category, subs, v) = if let Some(f2) = &f2 {
                let mut c: PClassInfo2 = std::mem::zeroed();
                if f2.getClassInfo2(i, &mut c) != kResultOk {
                    continue;
                }
                (text(&c.name), text(&c.category), text(&c.subCategories), text(&c.vendor))
            } else {
                let mut c: PClassInfo = std::mem::zeroed();
                if f.getClassInfo(i, &mut c) != kResultOk {
                    continue;
                }
                (text(&c.name), text(&c.category), String::new(), String::new())
            };
            if category != "Audio Module Class" {
                continue;
            }
            out.push(PluginInfo {
                name,
                vendor: if v.is_empty() { text(&vendor.vendor) } else { v },
                bundle: bundle.to_path_buf(),
                is_effect: subs.split('|').any(|s| s == "Fx") || !subs.split('|').any(|s| s == "Instrument"),
                class_index: i as usize,
            });
        }
    }
    Ok(out)
}

// ── what the host gives the plugin ───────────────────────────────────────────

/// Parameter edits the plugin itself makes (a knob moved in its own window); kept for later.
#[derive(Default)]
struct HostState {
    edits: Mutex<Vec<(u32, f64)>>,
}

struct Host(HostState);

impl Class for Host {
    type Interfaces = (IHostApplication, IComponentHandler);
}

impl IHostApplicationTrait for Host {
    unsafe fn getName(&self, name: *mut [u16; 128]) -> i32 {
        let n: Vec<u16> = "Dubplate".encode_utf16().collect();
        let dst = name as *mut u16;
        for (i, &c) in n.iter().enumerate() {
            *dst.add(i) = c;
        }
        *dst.add(n.len()) = 0;
        kResultOk
    }
    unsafe fn createInstance(&self, _cid: *mut [c_char; 16], _iid: *mut [c_char; 16], _obj: *mut *mut c_void) -> i32 {
        vst3::Steinberg::kNotImplemented
    }
}

impl IComponentHandlerTrait for Host {
    unsafe fn beginEdit(&self, _id: u32) -> i32 {
        kResultOk
    }
    unsafe fn performEdit(&self, id: u32, v: f64) -> i32 {
        self.0.edits.lock().unwrap().push((id, v));
        kResultOk
    }
    unsafe fn endEdit(&self, _id: u32) -> i32 {
        kResultOk
    }
    unsafe fn restartComponent(&self, _flags: i32) -> i32 {
        kResultOk
    }
}

/// One parameter's change for this block: the new value at the start.
struct Queue {
    id: u32,
    value: f64,
}

impl Class for Queue {
    type Interfaces = (IParamValueQueue,);
}

impl IParamValueQueueTrait for Queue {
    unsafe fn getParameterId(&self) -> u32 {
        self.id
    }
    unsafe fn getPointCount(&self) -> i32 {
        1
    }
    unsafe fn getPoint(&self, index: i32, offset: *mut i32, value: *mut f64) -> i32 {
        if index != 0 {
            return vst3::Steinberg::kInvalidArgument;
        }
        *offset = 0;
        *value = self.value;
        kResultOk
    }
    unsafe fn addPoint(&self, _o: i32, _v: f64, _i: *mut i32) -> i32 {
        vst3::Steinberg::kNotImplemented
    }
}

struct Changes {
    queues: Vec<ComWrapper<Queue>>,
}

impl Class for Changes {
    type Interfaces = (IParameterChanges,);
}

impl IParameterChangesTrait for Changes {
    unsafe fn getParameterCount(&self) -> i32 {
        self.queues.len() as i32
    }
    unsafe fn getParameterData(&self, index: i32) -> *mut IParamValueQueue {
        self.queues
            .get(index as usize)
            .and_then(|q| q.as_com_ref::<IParamValueQueue>())
            .map_or(std::ptr::null_mut(), |r| r.as_ptr())
    }
    unsafe fn addParameterData(&self, _id: *const u32, _index: *mut i32) -> *mut IParamValueQueue {
        std::ptr::null_mut()
    }
}

// ── the plugin ───────────────────────────────────────────────────────────────

/// Largest block handed to the plugin at once.
pub const MAX_BLOCK: usize = 1024;

/// A bus as the plugin is told about it each block: the first carries the stereo pair, any others are empty.
fn bus(main: bool) -> AudioBusBuffers {
    AudioBusBuffers {
        numChannels: if main { 2 } else { 0 },
        silenceFlags: 0,
        __field0: AudioBusBuffers__type0 { channelBuffers32: std::ptr::null_mut() },
    }
}

pub struct Plugin {
    pub info: PluginInfo,
    params: Vec<Param>,
    pending: Vec<(u32, f64)>,
    latency: u32,
    /// The bus descriptions handed to the plugin every block, made once (their buffer pointers are set per block).
    in_bus: Vec<AudioBusBuffers>,
    out_bus: Vec<AudioBusBuffers>,
    sample_rate: f64,
    tempo: f64,
    playing: bool,
    position: i64,
    in_buf: [Vec<f32>; 2],
    out_buf: [Vec<f32>; 2],
    // dropped in this order: the plugin's objects, then the library
    processor: ComPtr<IAudioProcessor>,
    component: ComPtr<IComponent>,
    controller: Option<ComPtr<IEditController>>,
    separate_controller: bool,
    _host: ComWrapper<Host>,
    _module: Module,
}

unsafe impl Send for Plugin {}

impl Plugin {
    /// Loads the plugin `class_index` of the bundle and gets it ready to process stereo at `sample_rate`.
    pub fn load(info: &PluginInfo, sample_rate: f64) -> Result<Plugin, String> {
        let module = Module::open(&info.bundle)?;
        unsafe {
            let f = module.factory();
            let mut cid: PClassInfo = std::mem::zeroed();
            if f.getClassInfo(info.class_index as i32, &mut cid) != kResultOk {
                return Err("the plugin has no such class".into());
            }
            let host = ComWrapper::new(Host(HostState::default()));
            let host_ctx = host.as_com_ref::<IHostApplication>().ok_or("host setup")?.as_ptr() as *mut FUnknown;

            let mut obj: *mut c_void = std::ptr::null_mut();
            let r = f.createInstance(cid.cid.as_ptr(), IComponent::IID.as_ptr() as *const c_char, &mut obj);
            let component = ComPtr::from_raw(obj as *mut IComponent)
                .filter(|_| r == kResultOk)
                .ok_or("the plugin would not make its audio component")?;
            if component.initialize(host_ctx) != kResultOk {
                return Err("the plugin failed to initialise".into());
            }

            // the controller: the component itself, or a second object the plugin names
            let (controller, separate) = match component.cast::<IEditController>() {
                Some(c) => (Some(c), false),
                None => {
                    let mut ccid = [0 as c_char; 16];
                    if component.getControllerClassId(&mut ccid) == kResultOk {
                        let mut o: *mut c_void = std::ptr::null_mut();
                        let r = f.createInstance(ccid.as_ptr(), IEditController::IID.as_ptr() as *const c_char, &mut o);
                        match ComPtr::from_raw(o as *mut IEditController).filter(|_| r == kResultOk) {
                            Some(c) => {
                                c.initialize(host_ctx);
                                (Some(c), true)
                            }
                            None => (None, false),
                        }
                    } else {
                        (None, false)
                    }
                }
            };
            if let Some(c) = &controller {
                if let Some(h) = host.as_com_ref::<IComponentHandler>() {
                    c.setComponentHandler(h.as_ptr());
                }
                if separate {
                    if let (Some(a), Some(b)) = (component.cast::<IConnectionPoint>(), c.cast::<IConnectionPoint>()) {
                        a.connect(b.as_ptr());
                        b.connect(a.as_ptr());
                    }
                }
            }

            let processor = component.cast::<IAudioProcessor>().ok_or("not an audio processor")?;
            let in_buses =
                component.getBusCount(MediaTypes_::kAudio as i32, BusDirections_::kInput as i32).max(0) as usize;
            let out_buses =
                component.getBusCount(MediaTypes_::kAudio as i32, BusDirections_::kOutput as i32).max(0) as usize;
            if out_buses == 0 {
                return Err("the plugin has no audio output".into());
            }
            let mut ins = vec![SpeakerArr::kStereo; in_buses];
            let mut outs = vec![SpeakerArr::kStereo; out_buses];
            processor.setBusArrangements(ins.as_mut_ptr(), in_buses as i32, outs.as_mut_ptr(), out_buses as i32);
            for i in 0..in_buses {
                component.activateBus(
                    MediaTypes_::kAudio as i32,
                    BusDirections_::kInput as i32,
                    i as i32,
                    (i == 0) as u8,
                );
            }
            for i in 0..out_buses {
                component.activateBus(
                    MediaTypes_::kAudio as i32,
                    BusDirections_::kOutput as i32,
                    i as i32,
                    (i == 0) as u8,
                );
            }
            if processor.canProcessSampleSize(SymbolicSampleSizes_::kSample32 as i32) != kResultOk {
                return Err("the plugin can't process 32-bit audio".into());
            }
            let mut setup = ProcessSetup {
                processMode: ProcessModes_::kRealtime as i32,
                symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
                maxSamplesPerBlock: MAX_BLOCK as i32,
                sampleRate: sample_rate,
            };
            if processor.setupProcessing(&mut setup) != kResultOk {
                return Err("the plugin refused this sample rate / block size".into());
            }
            component.setActive(1);
            processor.setProcessing(1);
            let latency = processor.getLatencySamples();

            let mut params = Vec::new();
            if let Some(c) = &controller {
                for i in 0..c.getParameterCount() {
                    let mut p: ParameterInfo = std::mem::zeroed();
                    if c.getParameterInfo(i, &mut p) != kResultOk {
                        continue;
                    }
                    if p.flags
                        & (ParameterInfo_::ParameterFlags_::kIsReadOnly | ParameterInfo_::ParameterFlags_::kIsHidden)
                        != 0
                    {
                        continue;
                    }
                    params.push(Param {
                        id: p.id,
                        name: text16(&p.title),
                        units: text16(&p.units),
                        steps: p.stepCount,
                        default: p.defaultNormalizedValue,
                    });
                }
            }
            Ok(Plugin {
                info: info.clone(),
                params,
                pending: Vec::new(),
                latency,
                in_bus: (0..in_buses).map(|i| bus(i == 0)).collect(),
                out_bus: (0..out_buses).map(|i| bus(i == 0)).collect(),
                sample_rate,
                tempo: 120.0,
                playing: true,
                position: 0,
                in_buf: [vec![0.0; MAX_BLOCK], vec![0.0; MAX_BLOCK]],
                out_buf: [vec![0.0; MAX_BLOCK], vec![0.0; MAX_BLOCK]],
                processor,
                component,
                controller,
                separate_controller: separate,
                _host: host,
                _module: module,
            })
        }
    }

    pub fn params(&self) -> &[Param] {
        &self.params
    }

    /// Delay the plugin adds, in frames: later audio comes out this many frames after it went in.
    pub fn latency_frames(&self) -> u32 {
        self.latency
    }

    /// The tempo and play state the plugin sees (tempo-synced effects follow it).
    pub fn set_transport(&mut self, bpm: f64, playing: bool) {
        self.tempo = bpm;
        self.playing = playing;
    }

    /// Sets a parameter (0..1) for the next block.
    pub fn set_param(&mut self, id: u32, normalized: f64) {
        let v = normalized.clamp(0.0, 1.0);
        if let Some(c) = &self.controller {
            unsafe {
                c.setParamNormalized(id, v);
            }
        }
        self.pending.retain(|p| p.0 != id);
        self.pending.push((id, v));
    }

    pub fn get_param(&self, id: u32) -> Option<f64> {
        self.controller.as_ref().map(|c| unsafe { c.getParamNormalized(id) })
    }

    /// Every listed parameter's current value (0..1), for saving and restoring.
    pub fn settings(&self) -> Vec<(u32, f64)> {
        self.params.iter().filter_map(|p| self.get_param(p.id).map(|v| (p.id, v))).collect()
    }

    /// "-6.0 dB", "1 kHz": the parameter's value as the plugin would show it.
    pub fn param_text(&self, id: u32, normalized: f64) -> String {
        let Some(c) = &self.controller else { return String::new() };
        let mut s = [0u16; 128];
        unsafe {
            if c.getParamStringByValue(id, normalized, &mut s) == kResultOk {
                text16(&s)
            } else {
                String::new()
            }
        }
    }

    /// Runs interleaved stereo audio through the plugin in place.
    pub fn process(&mut self, audio: &mut [f32]) {
        for chunk in audio.chunks_mut(MAX_BLOCK * 2) {
            let n = chunk.len() / 2;
            for i in 0..n {
                self.in_buf[0][i] = chunk[i * 2];
                self.in_buf[1][i] = chunk[i * 2 + 1];
            }
            self.run(n);
            for i in 0..n {
                chunk[i * 2] = self.out_buf[0][i];
                chunk[i * 2 + 1] = self.out_buf[1][i];
            }
        }
    }

    fn run(&mut self, n: usize) {
        unsafe {
            let mut in_ptrs = [self.in_buf[0].as_mut_ptr(), self.in_buf[1].as_mut_ptr()];
            let mut out_ptrs = [self.out_buf[0].as_mut_ptr(), self.out_buf[1].as_mut_ptr()];
            // nothing is allocated here unless a parameter was moved since the last block
            if let Some(b) = self.in_bus.first_mut() {
                b.__field0 = AudioBusBuffers__type0 { channelBuffers32: in_ptrs.as_mut_ptr() };
            }
            if let Some(b) = self.out_bus.first_mut() {
                b.__field0 = AudioBusBuffers__type0 { channelBuffers32: out_ptrs.as_mut_ptr() };
            }
            let (inputs, outputs) = (&mut self.in_bus, &mut self.out_bus);
            let changes = (!self.pending.is_empty()).then(|| {
                ComWrapper::new(Changes {
                    queues: self.pending.drain(..).map(|(id, value)| ComWrapper::new(Queue { id, value })).collect(),
                })
            });
            let changes_ptr = changes
                .as_ref()
                .and_then(|c| c.as_com_ref::<IParameterChanges>())
                .map_or(std::ptr::null_mut(), |r| r.as_ptr());
            let mut ctx: ProcessContext = std::mem::zeroed();
            // (the headers' enums are signed on Windows and unsigned elsewhere, hence the casts)
            ctx.state = ProcessContext_::StatesAndFlags_::kTempoValid
                | ProcessContext_::StatesAndFlags_::kTimeSigValid
                | if self.playing { ProcessContext_::StatesAndFlags_::kPlaying } else { 0 };
            ctx.sampleRate = self.sample_rate;
            ctx.projectTimeSamples = self.position;
            ctx.tempo = self.tempo;
            ctx.timeSigNumerator = 4;
            ctx.timeSigDenominator = 4;
            let mut data = ProcessData {
                processMode: ProcessModes_::kRealtime as i32,
                symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
                numSamples: n as i32,
                numInputs: inputs.len() as i32,
                numOutputs: outputs.len() as i32,
                inputs: if inputs.is_empty() { std::ptr::null_mut() } else { inputs.as_mut_ptr() },
                outputs: outputs.as_mut_ptr(),
                inputParameterChanges: changes_ptr,
                outputParameterChanges: std::ptr::null_mut(),
                inputEvents: std::ptr::null_mut(),
                outputEvents: std::ptr::null_mut(),
                processContext: &mut ctx,
            };
            self.processor.process(&mut data);
            self.position += n as i64;
        }
    }
}

impl Drop for Plugin {
    fn drop(&mut self) {
        unsafe {
            self.processor.setProcessing(0);
            self.component.setActive(0);
            if self.separate_controller {
                if let Some(c) = &self.controller {
                    c.terminate();
                }
            }
            self.component.terminate();
        }
    }
}

/// One effect on the main output, shared between the screen and the audio callback.
///
/// The audio thread only ever `try_lock`s, so it never waits: if the screen is in the middle of a parameter call the
/// block simply goes out without the effect.
pub struct MasterInsert {
    enabled: std::sync::atomic::AtomicBool,
    rate: std::sync::atomic::AtomicU32,
    plugin: Mutex<Option<Plugin>>,
}

impl Default for MasterInsert {
    fn default() -> Self {
        MasterInsert { enabled: false.into(), rate: 48000.into(), plugin: Mutex::new(None) }
    }
}

impl MasterInsert {}

impl MasterInsert {
    /// The output's sample rate. A plugin loaded for another rate is loaded again for this one.
    pub fn set_rate(&self, rate: u32) {
        use std::sync::atomic::Ordering::Relaxed;
        if self.rate.swap(rate, Relaxed) == rate {
            return;
        }
        let info = self.plugin.lock().unwrap().as_ref().map(|p| (p.info.clone(), p.settings()));
        if let Some((info, values)) = info {
            if let Ok(mut p) = Plugin::load(&info, rate as f64) {
                for (id, v) in values {
                    p.set_param(id, v);
                }
                let old = self.plugin.lock().unwrap().replace(p);
                drop(old);
            }
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(std::sync::atomic::Ordering::Relaxed)
    }
    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, std::sync::atomic::Ordering::Relaxed);
    }

    /// Loads a plugin into the slot (the old one is unloaded), ready to be switched on.
    pub fn load(&self, info: &PluginInfo) -> Result<(), String> {
        let p = Plugin::load(info, self.rate.load(std::sync::atomic::Ordering::Relaxed) as f64)?;
        let old = self.plugin.lock().unwrap().replace(p);
        drop(old);
        Ok(())
    }

    pub fn unload(&self) {
        let old = self.plugin.lock().unwrap().take();
        drop(old);
    }

    pub fn info(&self) -> Option<PluginInfo> {
        self.plugin.lock().unwrap().as_ref().map(|p| p.info.clone())
    }
    pub fn params(&self) -> Vec<Param> {
        self.plugin.lock().unwrap().as_ref().map(|p| p.params().to_vec()).unwrap_or_default()
    }
    pub fn latency_frames(&self) -> u32 {
        self.plugin.lock().unwrap().as_ref().map_or(0, |p| p.latency_frames())
    }
    pub fn set_param(&self, id: u32, v: f64) {
        if let Some(p) = self.plugin.lock().unwrap().as_mut() {
            p.set_param(id, v);
        }
    }
    pub fn get_param(&self, id: u32) -> Option<f64> {
        self.plugin.lock().unwrap().as_ref().and_then(|p| p.get_param(id))
    }
    pub fn param_text(&self, id: u32, v: f64) -> String {
        self.plugin.lock().unwrap().as_ref().map(|p| p.param_text(id, v)).unwrap_or_default()
    }
    /// Every parameter's current value, to save.
    pub fn settings(&self) -> Vec<(u32, f64)> {
        self.plugin.lock().unwrap().as_ref().map(|p| p.settings()).unwrap_or_default()
    }

    /// True when the effect will run on the next block.
    pub fn active(&self) -> bool {
        self.enabled() && self.plugin.lock().is_ok_and(|g| g.is_some())
    }

    /// The audio callback: runs interleaved stereo through the plugin if there is one and it is on. Never waits.
    pub fn process(&self, audio: &mut [f32]) {
        if !self.enabled() {
            return;
        }
        // The screen holds this lock for a few microseconds at a time while it reads parameter values (many times a
        // frame with the effect's window open). Giving up at once would send that block out without the effect, which
        // is heard as a click; so try for a short while first. A long hold (a plugin being swapped) is still skipped.
        for _ in 0..4000 {
            if let Ok(mut g) = self.plugin.try_lock() {
                if let Some(p) = g.as_mut() {
                    p.process(audio);
                }
                return;
            }
            std::hint::spin_loop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundles_are_found_a_few_folders_down() {
        let d = std::env::temp_dir().join(format!("dubplate-vst3-{}", std::process::id()));
        std::fs::create_dir_all(d.join("A.vst3/Contents")).unwrap();
        std::fs::create_dir_all(d.join("Vendor/B.vst3")).unwrap();
        std::fs::create_dir_all(d.join("Vendor/not-a-plugin")).unwrap();
        std::fs::create_dir_all(d.join("lib/vst3/C Effects.vst3/Contents/x86_64-linux/Inner.vst3")).unwrap(); // inside a bundle: ignored
        std::fs::create_dir_all(d.join("a/b/c/d/e/Too Deep.vst3")).unwrap();
        let found = find_bundles(&[d.clone(), d.join("lib")]);
        assert_eq!(
            found.iter().map(|p| p.file_name().unwrap().to_str().unwrap().to_string()).collect::<Vec<_>>(),
            ["A.vst3", "B.vst3", "C Effects.vst3"]
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn something_that_is_not_a_plugin_is_refused_without_a_crash() {
        let d = std::env::temp_dir().join(format!("dubplate-vst3-bad-{}", std::process::id()));
        let dir = d.join("Fake.vst3/Contents").join(format!("{}-linux", std::env::consts::ARCH));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Fake.so"), b"not a library").unwrap();
        assert!(inspect(&d.join("Fake.vst3")).is_err());
        assert!(inspect(&d.join("Missing.vst3")).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// With `DUBPLATE_TEST_VST3=/path/Some Effect.vst3`: loads it, bypass / effect behave, and it unloads cleanly.
    #[test]
    fn a_real_plugin_loads_processes_and_unloads() {
        let Some(path) = std::env::var_os("DUBPLATE_TEST_VST3") else { return };
        let infos = inspect(Path::new(&path)).unwrap();
        let info = infos.iter().find(|i| i.is_effect).expect("an effect in the bundle");
        let mut p = Plugin::load(info, 48000.0).unwrap();
        let src: Vec<f32> = (0..48000)
            .flat_map(|i| {
                let v = (i as f32 * 0.03).sin() * 0.3;
                [v, v]
            })
            .collect();
        let mut a = src.clone();
        p.process(&mut a);
        assert!(a.iter().all(|v| v.is_finite()));
        if let Some(b) = p.params().iter().find(|q| q.name.eq_ignore_ascii_case("bypass")) {
            p.set_param(b.id, 1.0);
            let mut c = src.clone();
            p.process(&mut c);
            let d: f32 = c.iter().zip(&src).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max);
            assert!(d < 1e-5, "bypassed output differs by {d}");
        }
        drop(p);
    }
}
