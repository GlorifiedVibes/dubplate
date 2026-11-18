mod app;
mod audio_settings;
mod cli;
mod db;
mod deck_settings;
mod deck_view;
mod flux;
mod grid_import;
mod importers;
mod library;
mod library_index;
mod library_query;
#[cfg(feature = "vst3-host")]
mod master_fx;
mod midi;
mod midi_learn;
mod midi_mapper;
mod midi_profile;
mod prepare;
mod sections;
mod stem_queue;
mod theme;
mod usb;
mod wave_draw;
mod waveform;
mod widgets;

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(code) = cli::run(&args) {
        std::process::exit(code);
    }
    env_logger::Builder::from_env(
        env_logger::Env::default()
            .default_filter_or("warn,wgpu_hal=error,wgpu_core=error,dubplate=info,dubplate_engine=info"),
    )
    .init();

    // DUBPLATE_SHOT_SIZE=1920x1080 sets the window size for a picture taken by the app (see `Shot` in app.rs)
    let size = std::env::var("DUBPLATE_SHOT_SIZE")
        .ok()
        .and_then(|v| {
            let (w, h) = v.split_once('x')?;
            Some([w.parse().ok()?, h.parse().ok()?])
        })
        .unwrap_or([1280.0, 800.0]);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Dubplate")
            .with_inner_size(size)
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };

    eframe::run_native("Dubplate", options, Box::new(|cc| Ok(Box::new(app::DubplateApp::new(cc)))))
}
