# Key lock and auto gain

Both are the usual DJ features (Master Tempo / key lock, and auto gain) and are built on published methods and open-source libraries.

## Key lock (the KEY button on each deck, on by default)

With it on, changing a deck's tempo does not change its pitch. With it off the pitch follows the tempo like a record.

- It uses Signalsmith Stretch (MIT) on the audio the playhead is about to reach. The stretcher answers later than it is asked, by `input latency + output latency x tempo`, so it is fed that far ahead and its sound lines up with the playhead. Cue points, loops, hot cues and the beat grid stay where they are.
- It runs only at a steady forward tempo between 0.25x and 2x that is not within 0.2 % of 1.0. Scratching, holding the jog, reverse and pitch-bend ramps use the plain playhead (the sound of a record), and the two are mixed over about 10 ms. After a jump or a loop wrap the plain signal plays for a few milliseconds and the stretcher is brought back in.
- Cost: about 1.4 % of one CPU core per deck while it is working, none at 1.0.
- Checked: a test places a click in a song and checks it is heard within 1.5 ms of the playhead reaching it, at three tempos; another checks a 1 kHz tone stays at 1 kHz at 1.08x (and rises to 1080 Hz without key lock); on real tracks (`cargo run -p dubplate-engine --release --example keylockcheck -- FILE 1.08`) the pitch shift read 0 cents at 1.08x and 0.92x (a speed change would be +133 / -144).
- Not checked: how it sounds. Phase-vocoder stretching of drums can soften transients at larger tempo changes; DJ programs commonly warn of the same beyond about 5 %. Listen to it before a gig.
- Not done yet: Key Shift / Key Sync (the stretcher can transpose, so this is next), a MIDI mapping for the button, saving the setting.

## Auto gain (the AUTO button under each GAIN knob, on by default)

- Loudness is measured when a track is loaded as ITU-R BS.1770-4 integrated loudness (LUFS: K-weighting, 400 ms blocks, absolute gate -70 LUFS, relative gate -10 LU), with a 4x oversampled true peak. About 1 ms per second of audio.
- Checked: the reading matches ffmpeg's EBU R128 meter on four real tracks to 0.1 LU, the EBU reference (997 Hz at -23 dBFS reads -23.0 LUFS), and is the same at 44.1, 48 and 96 kHz.
- Each deck gets a track gain that brings the track to -9 LUFS: cuts are always allowed; boosts only while the true peak stays within 1 dB of full scale; never more than 12 dB either way. The result is shown on the button and the GAIN knob stays yours. The master soft limiter still catches any overs.
- -9 LUFS is where club masters sit, so most tracks move by a few dB and only quiet or overloaded ones are corrected. It is a constant for now (`loudness::TARGET_LUFS`).
- Not done yet: a setting for the target, showing loudness in the library, saving the measure.
