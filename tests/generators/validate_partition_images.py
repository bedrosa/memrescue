#!/usr/bin/env python3
"""Validador independiente de las imágenes MBR/GPT del corpus.

Uso:
    python -I validate_partition_images.py <dir_imagenes> <dir_manifiestos_de_referencia>

No importa el generador: vuelve a interpretar cada imagen desde los bytes según la especificación
y compara con el manifiesto de referencia versionado en git (tests/corpus/manifests).
Comprueba también que la generación es determinista (SHA-256 igual al de referencia).

Código de salida 0 si todo es correcto; 1 si hay discrepancias.
"""
from __future__ import annotations

import hashlib
import struct
import sys
import tomllib
import uuid
import zlib
from pathlib import Path


class Report:
    def __init__(self, name: str) -> None:
        self.name, self.errors, self.checks = name, [], 0

    def check(self, cond: bool, msg: str) -> None:
        self.checks += 1
        if not cond:
            self.errors.append(msg)


def read_sector(data: bytes, ss: int, lba: int) -> bytes:
    return data[lba * ss : (lba + 1) * ss]


def parse_mbr_table(sector: bytes) -> list[tuple[int, int, int, int]]:
    """(boot_flag, tipo, lba_inicio, nº_sectores) de las 4 entradas."""
    out = []
    for i in range(4):
        e = sector[0x1BE + 16 * i : 0x1BE + 16 * (i + 1)]
        boot, ptype, start, count = e[0], e[4], *struct.unpack_from("<II", e, 8)
        out.append((boot, ptype, start, count))
    return out


def validate_mbr(data: bytes, m: dict, r: Report) -> None:
    ss = m["sector_size"]
    s0 = read_sector(data, ss, 0)
    r.check(s0[0x1FE:0x200] == b"\x55\xaa", "MBR sin firma 55AA")
    r.check(f"{struct.unpack_from('<I', s0, 0x1B8)[0]:08x}" == m["disk_signature"], "firma de disco distinta")
    found = []
    ext = None
    for boot, ptype, start, count in parse_mbr_table(s0):
        r.check(boot in (0x00, 0x80), f"indicador de arranque no válido: {boot:#x}")
        if ptype == 0:
            continue
        if ptype in (0x05, 0x0F, 0x85):
            ext = (start, count)
        else:
            found.append({"start_lba": start, "sectors": count, "mbr_type": ptype,
                          "active": boot == 0x80, "logical": False})
    r.check(ext == (m["extended_start_lba"], m["extended_sectors"]), f"partición extendida inesperada: {ext}")
    # Cadena de EBR: entrada 1 relativa al EBR actual; entrada 2 relativa al inicio de la extendida.
    ebrs, cur, seen = [], ext[0] if ext else None, set()
    while cur is not None:
        r.check(cur not in seen and ext[0] <= cur < ext[0] + ext[1], f"EBR fuera de rango o en bucle: {cur}")
        if cur in seen or not (ext[0] <= cur < ext[0] + ext[1]):
            break
        seen.add(cur)
        ebrs.append(cur)
        sec = read_sector(data, ss, cur)
        r.check(sec[0x1FE:0x200] == b"\x55\xaa", f"EBR en {cur} sin firma")
        t = parse_mbr_table(sec)
        b1, t1, s1, c1 = t[0]
        if t1:
            found.append({"start_lba": cur + s1, "sectors": c1, "mbr_type": t1,
                          "active": b1 == 0x80, "logical": True})
        _, t2, s2, _ = t[1]
        cur = ext[0] + s2 if t2 in (0x05, 0x0F) else None
    r.check(ebrs == m["ebr_lbas"], f"cadena de EBR {ebrs} != {m['ebr_lbas']}")
    compare_partitions(data, m, found, r, ["start_lba", "sectors", "mbr_type", "active", "logical"])


def guid(b: bytes) -> str:
    return str(uuid.UUID(bytes_le=b))


def parse_gpt_header(sec: bytes) -> dict:
    f = struct.unpack_from("<8sIIIIQQQQ16sQIII", sec, 0)
    keys = ["sig", "rev", "hsize", "crc", "res", "my", "alt", "first", "last",
            "guid", "entries_lba", "n", "esize", "ecrc"]
    h = dict(zip(keys, f))
    raw = bytearray(sec[: h["hsize"]])
    struct.pack_into("<I", raw, 16, 0)
    h["crc_ok"] = (zlib.crc32(bytes(raw)) & 0xFFFFFFFF) == h["crc"]
    h["guid"] = guid(h["guid"])
    return h


def validate_gpt(data: bytes, m: dict, r: Report) -> None:
    ss, total = m["sector_size"], m["total_sectors"]
    s0 = read_sector(data, ss, 0)
    pm = parse_mbr_table(s0)
    r.check(s0[0x1FE:0x200] == b"\x55\xaa", "MBR protector sin firma")
    r.check(pm[0][1] == 0xEE and pm[0][2] == 1 and pm[0][3] == min(total - 1, 0xFFFFFFFF),
            f"MBR protector incorrecto: {pm[0]}")
    r.check(all(e[1] == 0 for e in pm[1:]), "MBR protector con entradas adicionales")

    prim = parse_gpt_header(read_sector(data, ss, 1))
    back = parse_gpt_header(read_sector(data, ss, total - 1))
    for tag, h, my, alt in (("primaria", prim, 1, total - 1), ("copia", back, total - 1, 1)):
        r.check(h["sig"] == b"EFI PART", f"cabecera {tag}: firma")
        r.check(h["rev"] == 0x00010000, f"cabecera {tag}: revisión")
        r.check(92 <= h["hsize"] <= ss, f"cabecera {tag}: tamaño")
        r.check(h["crc_ok"], f"cabecera {tag}: CRC32 incorrecto")
        r.check((h["my"], h["alt"]) == (my, alt), f"cabecera {tag}: MyLBA/AlternateLBA")
        r.check(h["first"] == m["first_usable_lba"] and h["last"] == m["last_usable_lba"],
                f"cabecera {tag}: rango utilizable")
        r.check(h["guid"] == m["disk_guid"], f"cabecera {tag}: GUID de disco")
        r.check(128 <= h["esize"] and h["esize"] % 8 == 0, f"cabecera {tag}: tamaño de entrada")
    r.check(back["entries_lba"] == m["backup_entries_lba"], "LBA de entradas de la copia")

    tables = []
    for h in (prim, back):
        raw = data[h["entries_lba"] * ss : h["entries_lba"] * ss + h["n"] * h["esize"]]
        r.check((zlib.crc32(raw) & 0xFFFFFFFF) == h["ecrc"], "CRC32 de la tabla de entradas")
        tables.append(raw)
    r.check(tables[0] == tables[1], "la tabla de entradas de la copia difiere de la primaria")
    r.check(f"{prim['ecrc']:08x}" == m["entries_crc32"], "CRC32 de entradas distinto del manifiesto")

    found = []
    raw = tables[0]
    for i in range(prim["n"]):
        e = raw[i * prim["esize"] : (i + 1) * prim["esize"]]
        if e[:16] == b"\x00" * 16:
            continue
        start, end, _attrs = struct.unpack_from("<QQQ", e, 32)
        name = e[56:128].decode("utf-16-le").rstrip("\x00")
        r.check(prim["first"] <= start <= end <= prim["last"], f"entrada {i} fuera del rango utilizable")
        found.append({"start_lba": start, "sectors": end - start + 1, "end_lba": end,
                      "gpt_type": guid(e[0:16]), "unique_guid": guid(e[16:32]), "gpt_name": name})
    compare_partitions(data, m, found, r, ["start_lba", "sectors", "end_lba", "gpt_type", "unique_guid", "gpt_name"])


def compare_partitions(data: bytes, m: dict, found: list[dict], r: Report, keys: list[str]) -> None:
    exp = m.get("partitions", [])
    r.check(len(found) == len(exp), f"nº de particiones {len(found)} != {len(exp)}")
    ss = m["sector_size"]
    for f, e in zip(found, exp):
        for k in keys:
            r.check(f[k] == e[k], f"{e['label']}.{k}: {f[k]!r} != {e[k]!r}")
        mk = e["marker"].encode()
        sec = read_sector(data, ss, e["start_lba"])
        r.check(sec[: len(mk)] == mk, f"{e['label']}: marcador ausente en LBA {e['start_lba']}")


def main(argv: list[str]) -> int:
    sys.stdout.reconfigure(encoding="utf-8")
    if len(argv) != 2:
        print(__doc__)
        return 2
    img_dir, ref_dir = Path(argv[0]), Path(argv[1])
    refs = sorted(ref_dir.glob("*.manifest.toml"))
    if not refs:
        print(f"ERROR: no hay manifiestos de referencia en {ref_dir}")
        return 1
    failed = 0
    for ref in refs:
        m = tomllib.loads(ref.read_text(encoding="utf-8"))
        r = Report(m["name"])
        path = img_dir / m["file"]
        if not path.exists():
            r.errors.append(f"falta la imagen {path}")
        else:
            data = path.read_bytes()
            r.check(len(data) == m["size_bytes"], "tamaño distinto")
            r.check(len(data) == m["sector_size"] * m["total_sectors"], "tamaño no coincide con sectores")
            r.check(hashlib.sha256(data).hexdigest() == m["sha256"], "SHA-256 distinto (¿generación no determinista?)")
            {"mbr": validate_mbr, "gpt": validate_gpt}[m["scheme"]](data, m, r)
        status = "OK " if not r.errors else "FALLO"
        print(f"[{status}] {r.name}: {r.checks} comprobaciones, {len(r.errors)} errores")
        for e in r.errors:
            print(f"        - {e}")
        failed += bool(r.errors)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
