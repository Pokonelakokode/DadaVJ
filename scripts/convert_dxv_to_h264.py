#!/usr/bin/env python3
import os
import sys
import subprocess
import time
from pathlib import Path
from concurrent.futures import ProcessPoolExecutor, as_completed

def convert_video(mov_path: Path):
    mp4_path = mov_path.with_suffix('.mp4')
    tmp_path = mov_path.with_suffix('.tmp.mp4')
    
    start_t = time.time()
    orig_size = mov_path.stat().st_size
    
    # Run ffmpeg conversion
    cmd = [
        'ffmpeg', '-y', '-v', 'error',
        '-i', str(mov_path),
        '-map', '0:v:0',
        '-map', '0:a?',
        '-c:v', 'libx264',
        '-pix_fmt', 'yuv420p',
        '-crf', '18',
        '-preset', 'fast',
        '-g', '1',
        '-c:a', 'aac',
        '-b:a', '192k',
        str(tmp_path)
    ]
    res = subprocess.run(cmd, capture_output=True, text=True)
    if res.returncode != 0:
        if tmp_path.exists():
            tmp_path.unlink()
        return False, mov_path.name, f"FFmpeg hiba: {res.stderr.strip()}", 0, 0

    # Verify converted file with ffprobe
    probe_cmd = [
        'ffprobe', '-v', 'error',
        '-select_streams', 'v:0',
        '-show_entries', 'stream=codec_name',
        '-of', 'default=noprint_wrappers=1:nokey=1',
        str(tmp_path)
    ]
    probe_res = subprocess.run(probe_cmd, capture_output=True, text=True)
    if probe_res.returncode != 0 or 'h264' not in probe_res.stdout:
        if tmp_path.exists():
            tmp_path.unlink()
        return False, mov_path.name, "Ellenőrzés sikertelen (nincs h264 videósáv)", 0, 0

    new_size = tmp_path.stat().st_size
    # Atomically move tmp_path to mp4_path
    tmp_path.replace(mp4_path)
    
    # Delete original MOV file as requested
    mov_path.unlink()
    
    elapsed = time.time() - start_t
    return True, mov_path.name, f"{elapsed:.1f}s", orig_size, new_size

def main():
    videos_dir = Path("videos")
    if not videos_dir.exists():
        print("Hiba: A 'videos' mappa nem található!")
        sys.exit(1)
        
    mov_files = sorted([f for f in videos_dir.glob("*.mov") if not f.name.startswith(".")])
    total = len(mov_files)
    print(f"Összesen {total} db MOV videófájl található a 'videos' mappában.")
    if total == 0:
        print("Nincs konvertálandó fájl.")
        return

    workers = min(6, os.cpu_count() or 4)
    print(f"Konvertálás indítása {workers} párhuzamos szállal (H.264 All-Intra -g 1, CRF 18)...")

    success_count = 0
    fail_count = 0
    total_orig_bytes = 0
    total_new_bytes = 0
    
    with ProcessPoolExecutor(max_workers=workers) as executor:
        futures = {executor.submit(convert_video, f): f for f in mov_files}
        for i, future in enumerate(as_completed(futures), 1):
            ok, name, info, orig_sz, new_sz = future.result()
            if ok:
                success_count += 1
                total_orig_bytes += orig_sz
                total_new_bytes += new_sz
                saved_mb = (orig_sz - new_sz) / (1024 * 1024)
                print(f"[{i}/{total}] OK: {name} ({info}, méret: {orig_sz/(1024*1024):.1f}MB -> {new_sz/(1024*1024):.1f}MB)")
            else:
                fail_count += 1
                print(f"[{i}/{total}] HIBA: {name} - {info}")

    print("\n--- ÖSSZESÍTÉS ---")
    print(f"Sikeres konverzió: {success_count}/{total}")
    if fail_count > 0:
        print(f"Hibás: {fail_count}")
    print(f"Eredeti méret összesen: {total_orig_bytes / (1024*1024):.1f} MB")
    print(f"Új méret összesen: {total_new_bytes / (1024*1024):.1f} MB")
    saved_mb = (total_orig_bytes - total_new_bytes) / (1024*1024)
    print(f"Megtakarított tárhely: {saved_mb:.1f} MB ({saved_mb/1024:.2f} GB)")

if __name__ == '__main__':
    main()
