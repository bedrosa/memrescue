#!/usr/bin/env python3
"""Pruebas negativas: el validador debe detectar cada tipo de corrupción.

Uso: python -I test_validator.py <dir_imagenes> <dir_manifiestos>
Trabaja sobre copias en un directorio temporal; nunca modifica las imágenes originales.
"""
from __future__ import annotations

import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
VALIDATOR = HERE / "validate_partition_images.py"

# (imagen, desplazamiento en bytes, nuevos bytes, descripción)
MUTATIONS = [
    ("gpt_basic", 512 + 16, b"\x00\x00\x00\x00", "CRC de la cabecera GPT primaria"),
    # Byte bajo del LBA final de D1 en la tabla de la copia (0x27FF -> 0x2700).
    ("gpt_basic", 16777216 - 512 * 33 + 40, b"\x00", "tabla de entradas de la copia GPT"),
    ("gpt_basic", 512 + 0, b"XFI PART", "firma de la cabecera GPT"),
    ("gpt_basic", 0x1C2, b"\x07", "tipo del MBR protector"),
    ("gpt_4kn", 4096 * 2047 + 24, b"\x05", "MyLBA de la copia GPT 4Kn"),
    ("mbr_basic", 0x1FE, b"\x00\x00", "firma del MBR"),
    ("mbr_basic", 14336 * 512 + 0x1BE + 16 + 8, b"\x00\x10\x00\x00", "enlace de la cadena EBR"),
    ("mbr_basic", 0x1BE + 8, b"\x01\x08\x00\x00", "LBA de inicio de P1"),
    ("mbr_basic", 2048 * 512, b"XXXX", "marcador de la partición P1"),
]


def run(img_dir: Path, ref_dir: Path) -> int:
    rc = subprocess.run([sys.executable, "-I", "-X", "utf8", str(VALIDATOR), str(img_dir), str(ref_dir)],
                        capture_output=True, text=True, encoding="utf-8", errors="replace")
    return rc.returncode


def main(argv: list[str]) -> int:
    sys.stdout.reconfigure(encoding="utf-8")
    src, refs = Path(argv[0]), Path(argv[1])
    if run(src, refs) != 0:
        print("FALLO: las imágenes originales no validan; no se pueden hacer pruebas negativas")
        return 1
    failures = 0
    for name, off, payload, desc in MUTATIONS:
        with tempfile.TemporaryDirectory() as t:
            tdir, rdir = Path(t) / "img", Path(t) / "ref"
            tdir.mkdir()
            rdir.mkdir()
            shutil.copy(src / f"{name}.img", tdir / f"{name}.img")
            shutil.copy(refs / f"{name}.manifest.toml", rdir / f"{name}.manifest.toml")
            data = bytearray((tdir / f"{name}.img").read_bytes())
            data[off : off + len(payload)] = payload
            (tdir / f"{name}.img").write_bytes(data)
            detected = run(tdir, rdir) != 0
            print(f"[{'OK ' if detected else 'FALLO'}] detecta corrupción: {desc}")
            failures += not detected
    print(f"{len(MUTATIONS) - failures}/{len(MUTATIONS)} corrupciones detectadas")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
