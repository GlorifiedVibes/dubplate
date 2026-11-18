#!/usr/bin/env python3
"""
Lists the beat grid (PQTZ) and phrase tag (PSSI) that rekordbox stored for each analysed track, as ground truth
for checking Dubplate's beat tracker.

    python3 scripts/anlz_dump.py [USBANLZ_DIR]

Default folder: the Windows rekordbox install seen through a mounted partition, or pass your own
(a USB stick exported from rekordbox has PIONEER/USBANLZ/...). Format: https://djl-analysis.deepsymmetry.org/
"""
import glob, os, struct, sys

def tags(path):
    d = open(path, 'rb').read()
    if d[:4] != b'PMAI':
        return
    pos = struct.unpack('>I', d[4:8])[0]
    while pos + 12 <= len(d):
        fcc = d[pos:pos + 4].decode('latin1')
        hl, tl = struct.unpack('>II', d[pos + 4:pos + 12])
        yield fcc, d[pos:pos + tl]
        if tl == 0:
            break
        pos += tl

def main():
    if len(sys.argv) < 2:
        sys.exit('usage: anlz_dump.py FOLDER   (a USBANLZ folder: PIONEER/USBANLZ on a stick, or the same folder in the program\'s data directory)')
    root = sys.argv[1]
    for dat in sorted(glob.glob(root + '/**/ANLZ0000.DAT', recursive=True)):
        path, beats, phrases = '?', [], None
        for fcc, b in tags(dat):
            if fcc == 'PPTH':
                n = struct.unpack('>I', b[12:16])[0]
                path = b[16:16 + n].decode('utf-16-be').rstrip('\x00')
            if fcc == 'PQTZ':
                n = struct.unpack('>I', b[20:24])[0]
                beats = [struct.unpack('>HHI', b[24 + 8 * i:32 + 8 * i]) for i in range(n)]   # (beat in bar, bpm*100, ms)
        ext = dat[:-3] + 'EXT'
        if os.path.exists(ext):
            phrases = next((b for f, b in tags(ext) if f == 'PSSI'), None)
        first = beats[0] if beats else None
        print(f"{os.path.basename(path)}\tbeats {len(beats)}\tfirst {first}\ttempos {sorted(set(x[1] for x in beats))[:3]}\tphrase tag {'yes' if phrases else 'no'}")

if __name__ == '__main__':
    main()
