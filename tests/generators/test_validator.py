#!/usr/bin/env python3
"""Pruebas negativas: el validador debe detectar cada tipo de corrupción.

Uso: python -I test_validator.py <dir_imagenes> <dir_manifiestos>
Trabaja sobre copias en un directorio temporal; nunca modifica las imágenes originales.

En cada caso se altera la imagen (o la verdad esperada del manifiesto) y se **actualiza** el SHA-256
de la copia del manifiesto, de modo que la corrupción solo puede detectarse por las comprobaciones
estructurales del validador y no por el hash.
"""
from __future__ import annotations

import hashlib
import re
import struct
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
VALIDATOR = HERE / "validate_partition_images.py"
S = 512


def q(v: int) -> bytes:
    return struct.pack("<Q", v)


def d(v: int) -> bytes:
    return struct.pack("<I", v)


# (imagen, [(desplazamiento en bytes, nuevos bytes)], descripción)
IMAGE_MUTATIONS = [
    ("gpt_basic", [(S + 16, b"\x00\x00\x00\x00")], "CRC de la cabecera GPT primaria"),
    # Byte bajo del LBA final de D1 en la tabla de la copia (0x27FF -> 0x2700).
    ("gpt_basic", [(16777216 - S * 33 + 40, b"\x00")], "tabla de entradas de la copia GPT"),
    ("gpt_basic", [(S, b"XFI PART")], "firma de la cabecera GPT"),
    ("gpt_basic", [(0x1C2, b"\x07")], "tipo del MBR protector"),
    ("gpt_4kn", [(4096 * 2047 + 24, b"\x05")], "MyLBA de la copia GPT 4Kn"),
    ("mbr_basic", [(0x1FE, b"\x00\x00")], "firma del MBR"),
    ("mbr_basic", [(14336 * S + 0x1BE + 16 + 8, d(0x1000))], "enlace de la cadena EBR"),
    ("mbr_basic", [(0x1BE + 8, d(0x801))], "LBA de inicio de P1"),
    ("mbr_basic", [(2048 * S, b"XXXX")], "marcador de la partición P1"),
    # MBR/EBR dañados o límite
    ("mbr_no_signature", [(0x1FE, b"\x55\xaa")], "firma 0x55AA añadida (desaparece la anomalía)"),
    ("mbr_two_active", [(0x1BE + 16, b"\x00")], "segunda entrada ya no activa"),
    ("mbr_ext_0x85", [(0x1BE + 16 + 4, b"\x0f")], "tipo de la extendida 0x85 -> 0x0F"),
    ("mbr_ebr_loop", [(3072 * S + 0x1BE + 16 + 4, b"\x00")], "cadena de EBR sin bucle"),
    ("mbr_ebr_link_outside", [(2560 * S + 0x1BE + 16 + 8, d(512))], "enlace de EBR dentro del disco"),
    ("mbr_overlap", [(0x1BE + 16 + 8, d(3072))], "entradas sin solape"),
    ("mbr_unknown_type", [(0x1BE + 16 + 4, b"\x83")], "tipo desconocido sustituido por uno conocido"),
    ("mbr_outside_disk", [(0x1BE + 16 + 12, d(1024))], "entrada recortada dentro del disco"),
    ("empty_disk", [(0x1FE, b"\x55\xaa")], "firma 0x55AA en un disco vacío"),
    ("protective_mbr_only", [(S, b"EFI PART")], "firma GPT en LBA 1 del disco sin cabeceras"),
    ("hybrid_mbr_gpt", [(0x1BE + 16 + 8, d(2049))], "entrada espejo del MBR híbrido desalineada"),
    # GPT dañadas, desplazadas o recortadas
    ("gpt_primary_header_bad", [(4095 * S + 48, b"\x00")], "respaldo también dañado"),
    ("gpt_entries_crc_bad", [(4063 * S + 40, b"\x00")], "tabla de entradas del respaldo alterada"),
    ("gpt_backup_header_bad", [(S + 48, b"\x00")], "cabecera primaria también dañada"),
    ("gpt_backup_missing", [(4095 * S, b"EFI PART")], "firma GPT en la posición del respaldo borrado"),
    ("gpt_both_bad", [(0x1C2, b"\x07")], "tipo del MBR protector"),
    ("gpt_4kn_primary_bad", [(511 * 4096 + 48, b"\x00")], "respaldo 4Kn también dañado"),
    ("gpt_shifted_start", [(4032 * S + 24, q(4096))], "MyLBA de la copia (desplazamiento distinto)"),
    ("gpt_shifted_start_1", [(48, b"\x00")], "cabecera primaria desplazada dañada"),
    ("gpt_truncated_end", [(0x1BE + 12, d(3895))], "tamaño del MBR protector ajustado a la imagen recortada"),
    # Volúmenes perdidos
    ("lost_ntfs_mbr_zeroed", [(4095 * S + 0x48, b"\xff")], "número de serie de la copia NTFS"),
    ("lost_fat32_chs", [((63 + 6) * S + 0x52, b"X")], "copia FAT32 del sector 6"),
    ("lost_exfat_1mib", [((2048 + 11) * S, b"\x00")], "suma de comprobación de la región exFAT principal"),
    ("lost_chain_three", [(71687 * S + 64, q(71688))], "PartitionOffset del exFAT sin alinear"),
    ("ntfs_primary_damaged", [(4095 * S, b"\x00" * S)], "copia NTFS también borrada"),
    ("fat32_primary_damaged", [((2048 + 1) * S, b"\x00" * 4)], "FSInfo de FAT32"),
    ("exfat_primary_damaged", [((2048 + 13) * S + 510, b"\x00")], "firma de un sector de arranque extendido del respaldo"),
    ("primary_unreadable", [(2048 * S + 3, b"XXXX")], "relleno de sector ilegible"),
    ("backup_unreadable", [(4095 * S + 3, b"XXXX")], "relleno de sector ilegible de la copia"),
    ("image_shifted_pos", [(8 * S + 0x1BE + 8, d(2047))], "MBR desplazado que ya no coincide con los volúmenes"),
    ("image_shifted_neg", [(2008 * S, b"\x00" * S)], "respaldo exFAT (única prueba del desplazamiento)"),
    ("gpt_4kn_lost", [(511 * 4096 + 0x48, b"\xff")], "número de serie de la copia NTFS 4Kn"),
    ("superfloppy", [(0x1FE, b"\x00\x00")], "firma del arranque NTFS en LBA 0"),
    ("random_disk", [(0x1BE + 16 * i, b"\x00") for i in range(4)], "datos aleatorios convertidos en tabla"),
    ("fake_mbr_in_data", [(2112 * S + 0x1FE, b"\x00\x00")], "señuelo sin aspecto de tabla"),
]

# (imagen, texto original, texto nuevo, descripción): la verdad esperada del manifiesto es falsa.
MANIFEST_MUTATIONS = [
    ("mbr_ebr_loop", "target_lba = 2560", "target_lba = 2561", "destino del bucle de EBR"),
    ("mbr_unknown_type", 'partitions = ["P2"]', 'partitions = ["P1"]', "partición con tipo desconocido"),
    ("gpt_primary_header_bad", 'gpt_selected = "backup"', 'gpt_selected = "primary"', "copia GPT elegida"),
    ("gpt_truncated_end", "trailing_sectors_missing = 200", "trailing_sectors_missing = 0", "sectores finales ausentes"),
    ("image_shifted_neg", "leading_sectors_missing = 2100", "leading_sectors_missing = 2099", "desplazamiento"),
    ("lost_chain_three", 'primary_boot = "valid"', 'primary_boot = "damaged"', "estado del arranque principal"),
    ("primary_unreadable", 'primary_boot = "unreadable"', 'primary_boot = "damaged"', "ilegible frente a dañado"),
    ("hybrid_mbr_gpt", 'gpt_label = "L1"', 'gpt_label = "D1"', "correspondencia del MBR híbrido"),
    ("protective_mbr_only", 'code = "gpt_backup_header_missing"', 'code = "gpt_backup_header_crc"', "anomalía GPT"),
    ("lost_fat32_chs", "suggested_mbr_type = 12", "suggested_mbr_type = 11", "tipo MBR sugerido"),
]


def run(img_dir: Path, ref_dir: Path) -> int:
    rc = subprocess.run([sys.executable, "-I", "-X", "utf8", str(VALIDATOR), str(img_dir), str(ref_dir)],
                        capture_output=True, text=True, encoding="utf-8", errors="replace")
    return rc.returncode


def detected(src: Path, refs: Path, name: str, patches: list, text_patch: tuple | None) -> bool:
    with tempfile.TemporaryDirectory() as t:
        tdir, rdir = Path(t) / "img", Path(t) / "ref"
        tdir.mkdir()
        rdir.mkdir()
        data = bytearray((src / f"{name}.img").read_bytes())
        for off, payload in patches:
            assert data[off : off + len(payload)] != payload, f"{name}: la mutación en {off} no cambia nada"
            data[off : off + len(payload)] = payload
        (tdir / f"{name}.img").write_bytes(data)
        text = (refs / f"{name}.manifest.toml").read_text(encoding="utf-8")
        if text_patch:
            assert text_patch[0] in text, f"{name}: no se encuentra {text_patch[0]!r}"
            text = text.replace(text_patch[0], text_patch[1], 1)
        text = re.sub(r'(?m)^sha256 = ".*"$', f'sha256 = "{hashlib.sha256(data).hexdigest()}"', text)
        (rdir / f"{name}.manifest.toml").write_text(text, encoding="utf-8", newline="\n")
        return run(tdir, rdir) != 0


def main(argv: list[str]) -> int:
    sys.stdout.reconfigure(encoding="utf-8")
    src, refs = Path(argv[0]), Path(argv[1])
    if run(src, refs) != 0:
        print("FALLO: las imágenes originales no validan; no se pueden hacer pruebas negativas")
        return 1
    failures = total = 0
    # Control: una copia sin cambios con el manifiesto reescrito debe seguir validando.
    for name in ("mbr_basic", "gpt_shifted_start", "lost_chain_three"):
        total += 1
        ok = not detected(src, refs, name, [], None)
        print(f"[{'OK ' if ok else 'FALLO'}] copia sin cambios de {name} valida")
        failures += not ok
    for name, patches, desc in IMAGE_MUTATIONS:
        total += 1
        ok = detected(src, refs, name, patches, None)
        print(f"[{'OK ' if ok else 'FALLO'}] detecta corrupción ({name}): {desc}")
        failures += not ok
    for name, old, new, desc in MANIFEST_MUTATIONS:
        total += 1
        ok = detected(src, refs, name, [], (old, new))
        print(f"[{'OK ' if ok else 'FALLO'}] detecta manifiesto falso ({name}): {desc}")
        failures += not ok
    print(f"{total - failures}/{total} casos correctos ({len(IMAGE_MUTATIONS)} corrupciones de imagen, "
          f"{len(MANIFEST_MUTATIONS)} de manifiesto, 3 controles)")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
