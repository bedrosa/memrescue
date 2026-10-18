#!/usr/bin/env python3
"""Validador estructural independiente del corpus NTFS descargado.

Uso:
    python -I -X utf8 tests/generators/validate_ntfs_images.py <dir_imagenes> <dir_manifiestos>
    (normalmente tests/corpus/ntfs/generated y tests/corpus/ntfs/manifests)

No importa el generador ni usa The Sleuth Kit (los manifiestos ya salen de TSK): vuelve a leer cada
imagen desde los bytes según la estructura pública de NTFS y de las tablas MBR/GPT y comprueba que es
coherente con su manifiesto versionado:
  - tamaño y SHA-256 de la imagen;
  - MBR (firma de disco, entrada, tipo, activa) o GPT (cabeceras principal y de respaldo con CRC-32,
    tabla de entradas, GUID, tipo, LBA y nombre);
  - sector de arranque NTFS (OEM, firma 0x55AA, geometría, sectores ocultos, LCN de $MFT y $MFTMirr,
    número de serie, tamaños de registro e índice) y su copia en la última LBA del volumen;
  - registros 0 a 3 de $MFT con firma FILE y su espejo $MFTMirr;
  - para cada entrada: firma FILE (tras aplicar la secuencia de actualización), número de registro,
    secuencia, indicadores en uso y directorio, y para cada flujo $DATA (con y sin nombre) residencia,
    tamaño, runs y dispersos decodificados de los mapping pairs y SHA-256 de los datos leídos de la
    imagen siguiendo esos runs;
  - la corrupción de los escenarios dañados (arranque principal, registros MFT, $MFTMirr).

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

SCHEMA_VERSION = 1


class Report:
    def __init__(self) -> None:
        self.errors: list[str] = []
        self.checks = 0
        self.skipped = 0

    def check(self, cond: bool, msg: str) -> bool:
        self.checks += 1
        if not cond:
            self.errors.append(msg)
        return cond


# --------------------------------------------------------------------------------- tablas


def check_mbr(r: Report, img: bytes, p: dict) -> None:
    s = img[:512]
    r.check(s[510:512] == b"\x55\xaa", "MBR sin firma 0x55AA")
    r.check(s[0x1B8:0x1BC][::-1].hex() == p["disk_signature"], "firma de disco MBR distinta")
    e = s[0x1BE:0x1CE]
    start, count = struct.unpack_from("<II", e, 8)
    r.check(e[4] == p["mbr_type"], f"tipo MBR {e[4]:#x}")
    r.check((e[0] == 0x80) == p["active"], "indicador de activa distinto")
    r.check((start, count) == (p["start_lba"], p["sectors"]), f"entrada MBR {start}+{count}")
    r.check(s[0x1CE:0x1FE] == bytes(48), "entradas MBR 2 a 4 no vacías")


def gpt_header(r: Report, img: bytes, lba: int, which: str) -> dict | None:
    h = img[lba * 512:lba * 512 + 512]
    if not r.check(h[:8] == b"EFI PART", f"GPT {which}: sin firma"):
        return None
    size = struct.unpack_from("<I", h, 12)[0]
    crc = struct.unpack_from("<I", h, 16)[0]
    body = bytearray(h[:size])
    body[16:20] = bytes(4)
    r.check(zlib.crc32(body) == crc, f"GPT {which}: CRC-32 de cabecera")
    return {
        "current": struct.unpack_from("<Q", h, 24)[0], "backup": struct.unpack_from("<Q", h, 32)[0],
        "guid": str(uuid.UUID(bytes_le=h[56:72])), "entries_lba": struct.unpack_from("<Q", h, 72)[0],
        "count": struct.unpack_from("<I", h, 80)[0], "entry_size": struct.unpack_from("<I", h, 84)[0],
        "entries_crc": struct.unpack_from("<I", h, 88)[0],
    }


def check_gpt(r: Report, img: bytes, p: dict) -> None:
    s = img[:512]
    r.check(s[510:512] == b"\x55\xaa" and s[0x1BE + 4] == 0xEE, "MBR protector ausente")
    last = len(img) // 512 - 1
    for which, lba in (("principal", 1), ("respaldo", last)):
        h = gpt_header(r, img, lba, which)
        if h is None:
            continue
        r.check(h["current"] == lba, f"GPT {which}: LBA propia {h['current']}")
        r.check(h["guid"] == p["disk_guid"], f"GPT {which}: GUID de disco {h['guid']}")
        table = img[h["entries_lba"] * 512:h["entries_lba"] * 512 + h["count"] * h["entry_size"]]
        r.check(zlib.crc32(table) == h["entries_crc"], f"GPT {which}: CRC-32 de entradas")
        e = table[:h["entry_size"]]
        first, end = struct.unpack_from("<QQ", e, 32)
        r.check(str(uuid.UUID(bytes_le=e[:16])) == p["gpt_type"], f"GPT {which}: tipo")
        r.check(str(uuid.UUID(bytes_le=e[16:32])) == p["unique_guid"], f"GPT {which}: GUID único")
        r.check((first, end) == (p["start_lba"], p["start_lba"] + p["sectors"] - 1), f"GPT {which}: LBA {first}-{end}")
        r.check(e[56:128].decode("utf-16-le").rstrip("\0") == p["gpt_name"], f"GPT {which}: nombre")
        r.check(not any(table[h["entry_size"]:]), f"GPT {which}: entradas adicionales")


# ---------------------------------------------------------------------------------- NTFS


def boot_ok(s: bytes) -> bool:
    return len(s) == 512 and s[3:11] == b"NTFS    " and s[510:512] == b"\x55\xaa"


def check_boot(r: Report, s: bytes, v: dict, hidden: int, which: str) -> None:
    if not r.check(boot_ok(s), f"arranque {which}: OEM o firma"):
        return
    bps = struct.unpack_from("<H", s, 0x0B)[0]
    spc = s[0x0D]
    rec = struct.unpack_from("<b", s, 0x40)[0]
    idx = struct.unpack_from("<b", s, 0x44)[0]
    cs = bps * spc
    size = lambda raw: raw * cs if raw > 0 else 1 << -raw  # noqa: E731
    r.check(bps == v["sector_size"], f"arranque {which}: bytes por sector {bps}")
    r.check(cs == v["cluster_size"], f"arranque {which}: cluster {cs}")
    r.check(struct.unpack_from("<I", s, 0x1C)[0] == hidden, f"arranque {which}: sectores ocultos")
    r.check(struct.unpack_from("<Q", s, 0x28)[0] == v["total_sectors"], f"arranque {which}: total de sectores")
    r.check(struct.unpack_from("<Q", s, 0x30)[0] == v["mft_lcn"], f"arranque {which}: LCN de $MFT")
    r.check(struct.unpack_from("<Q", s, 0x38)[0] == v["mftmirr_lcn"], f"arranque {which}: LCN de $MFTMirr")
    r.check(size(rec) == v["mft_record_size"], f"arranque {which}: tamaño de registro")
    r.check(size(idx) == v["index_record_size"], f"arranque {which}: tamaño de índice")
    r.check(struct.unpack_from("<Q", s, 0x48)[0] == int(v["serial"], 16), f"arranque {which}: número de serie")


def apply_fixup(rec: bytes, sector: int) -> bytes | None:
    usa_off, usa_n = struct.unpack_from("<HH", rec, 4)
    if usa_n == 0 or usa_off + 2 * usa_n > len(rec) or (usa_n - 1) * sector > len(rec):
        return None
    b = bytearray(rec)
    usn = b[usa_off:usa_off + 2]
    for i in range(1, usa_n):
        end = i * sector - 2
        if b[end:end + 2] != usn:
            return None
        b[end:end + 2] = b[usa_off + 2 * i:usa_off + 2 * i + 2]
    return bytes(b)


def decode_runs(data: bytes) -> list[tuple[int | None, int]]:
    out, pos, lcn = [], 0, 0
    while pos < len(data) and data[pos]:
        hdr = data[pos]
        nl, no = hdr & 0x0F, hdr >> 4
        length = int.from_bytes(data[pos + 1:pos + 1 + nl], "little")
        if no:
            lcn += int.from_bytes(data[pos + 1 + nl:pos + 1 + nl + no], "little", signed=True)
            out.append((lcn, length))
        else:
            out.append((None, length))
        pos += 1 + nl + no
    return out


def extents(pieces: list[tuple[int | None, int]]) -> tuple[list[list[int]], list[list[int]]]:
    merged: list[list] = []
    vcn = 0
    for lcn, n in pieces:
        if merged and ((merged[-1][1] is None and lcn is None) or
                       (merged[-1][1] is not None and lcn is not None and merged[-1][1] + merged[-1][2] == lcn)):
            merged[-1][2] += n
        else:
            merged.append([vcn, lcn, n])
        vcn += n
    return [[a, b, c] for a, b, c in merged if b is not None], [[a, c] for a, b, c in merged if b is None]


def parse_record(rec: bytes) -> dict:
    """Atributos de un registro MFT ya corregido: tipo, nombre, residencia, valor o runs."""
    out = {"seq": struct.unpack_from("<H", rec, 0x10)[0], "flags": struct.unpack_from("<H", rec, 0x16)[0],
           "number": struct.unpack_from("<I", rec, 0x2C)[0], "attrs": []}
    pos = struct.unpack_from("<H", rec, 0x14)[0]
    while pos + 16 <= len(rec):
        typ, length = struct.unpack_from("<II", rec, pos)
        if typ == 0xFFFFFFFF or length < 16 or pos + length > len(rec):
            break
        nonres, name_len, name_off = rec[pos + 8], rec[pos + 9], struct.unpack_from("<H", rec, pos + 10)[0]
        name = rec[pos + name_off:pos + name_off + 2 * name_len].decode("utf-16-le")
        a = {"type": typ, "name": name, "resident": nonres == 0}
        if nonres == 0:
            vlen, voff = struct.unpack_from("<IH", rec, pos + 16)
            a["value"] = rec[pos + voff:pos + voff + vlen]
        else:
            start_vcn, _last, runs_off = struct.unpack_from("<QQH", rec, pos + 16)
            a["start_vcn"] = start_vcn
            a["size"], a["init_size"] = struct.unpack_from("<QQ", rec, pos + 48)
            a["pieces"] = decode_runs(rec[pos + runs_off:pos + length])
        out["attrs"].append(a)
        pos += length
    return out


class Volume:
    def __init__(self, img: bytes, v: dict) -> None:
        self.img, self.v = img, v
        self.base, self.cs, self.rs = v["offset"], v["cluster_size"], v["mft_record_size"]

    def record_raw(self, n: int) -> bytes | None:
        pos = n * self.rs
        for vcn, lcn, count in self.v["mft_runs"]:
            if vcn * self.cs <= pos < (vcn + count) * self.cs:
                at = self.base + lcn * self.cs + pos - vcn * self.cs
                return self.img[at:at + self.rs]
        return None

    def read_stream(self, runs: list[list[int]], size: int, init_size: int) -> bytes:
        buf = bytearray(-(-size // self.cs) * self.cs)
        for vcn, lcn, count in runs:
            start = self.base + lcn * self.cs
            chunk = self.img[start:start + count * self.cs]
            buf[vcn * self.cs:vcn * self.cs + len(chunk)] = chunk
        buf[init_size:] = bytes(len(buf) - init_size)
        return bytes(buf[:size])


def check_stream(r: Report, vol: Volume, where: str, parts: list[dict], want: dict, expect_sha: str | None) -> None:
    resident = parts[0]["resident"]
    r.check(resident == want["resident"], f"{where}: residente={resident}")
    if resident:
        value = parts[0]["value"]
        r.check(len(value) == want["size"], f"{where}: tamaño residente {len(value)}")
        if expect_sha:
            r.check(hashlib.sha256(value).hexdigest() == expect_sha, f"{where}: SHA-256 del valor residente")
        return
    parts = sorted(parts, key=lambda a: a["start_vcn"])
    first = parts[0]
    if not r.check(first["start_vcn"] == 0, f"{where}: falta el primer tramo del atributo (lista de atributos)"):
        return
    pieces = [p for a in parts for p in a["pieces"]]
    runs, sparse = extents(pieces)
    r.check(first["size"] == want["size"], f"{where}: tamaño {first['size']}")
    r.check(runs == want.get("runs", []), f"{where}: runs {runs} != {want.get('runs', [])}")
    r.check(sparse == want.get("sparse", []), f"{where}: dispersos {sparse} != {want.get('sparse', [])}")
    r.check(first["init_size"] == want.get("init_size", want["size"]), f"{where}: init_size {first['init_size']}")
    if expect_sha:
        data = vol.read_stream(runs, first["size"], first["init_size"])
        r.check(hashlib.sha256(data).hexdigest() == expect_sha, f"{where}: SHA-256 de los datos leídos por runs")


def check_files(r: Report, vol: Volume, m: dict) -> None:
    sector = vol.v["sector_size"]
    for f in m["files"]:
        where = f"{f['path']} (registro {f['mft_record']})"
        raw = vol.record_raw(f["mft_record"])
        if not r.check(raw is not None, f"{where}: fuera de los runs de $MFT"):
            continue
        if f.get("mft_damaged"):
            r.check(raw[:4] != b"FILE", f"{where}: debía estar dañado y tiene firma FILE")
            continue
        if not r.check(raw[:4] == b"FILE", f"{where}: sin firma FILE"):
            continue
        rec = apply_fixup(raw, sector)
        if not r.check(rec is not None, f"{where}: secuencia de actualización incoherente"):
            continue
        p = parse_record(rec)
        r.check(p["number"] == f["mft_record"], f"{where}: número de registro {p['number']}")
        r.check(p["seq"] == f["sequence"], f"{where}: secuencia {p['seq']}")
        r.check(bool(p["flags"] & 1) == (f["state"] == "live"), f"{where}: indicador en uso {p['flags']:#x}")
        r.check(bool(p["flags"] & 2) == (f["type"] == "dir"), f"{where}: indicador de directorio {p['flags']:#x}")
        if any(a["type"] == 0x20 for a in p["attrs"]):
            r.skipped += 1
            continue
        streams: dict[str, list[dict]] = {}
        for a in p["attrs"]:
            if a["type"] == 0x80:
                streams.setdefault(a["name"], []).append(a)
        if f["type"] == "file":
            if r.check("" in streams, f"{where}: sin $DATA"):
                sha = f["sha256"] if f["recoverable"] == "full" else f.get("recovered_sha256")
                check_stream(r, vol, where, streams[""], f, sha)
        want_ads = {a["name"]: a for a in f.get("ads", [])}
        r.check(sorted(n for n in streams if n) == sorted(want_ads), f"{where}: flujos {sorted(n for n in streams if n)}")
        for name in sorted(set(streams) & set(want_ads)):
            check_stream(r, vol, f"{where}:{name}", streams[name], want_ads[name], want_ads[name]["sha256"])


def validate(img_path: Path, m: dict) -> Report:
    r = Report()
    r.check(m.get("schema_version") == SCHEMA_VERSION, f"schema_version {m.get('schema_version')}")
    if not r.check(img_path.is_file(), f"falta {img_path.name}"):
        return r
    img = img_path.read_bytes()
    r.check(len(img) == m["image_size"], f"tamaño {len(img)} != {m['image_size']}")
    r.check(hashlib.sha256(img).hexdigest() == m["image_sha256"], "SHA-256 de la imagen distinto del manifiesto")
    v = m["volume"]
    hidden = 0
    if "partition" in m:
        p = m["partition"]
        r.check(v["offset"] == p["start_lba"] * 512, "el volumen no empieza en la partición")
        hidden = p["start_lba"]
        (check_mbr if p["scheme"] == "mbr" else check_gpt)(r, img, p)
    damage = {d["kind"]: d for d in m.get("damage", [])}
    primary = img[v["offset"]:v["offset"] + 512]
    backup = img[v["backup_boot_offset"]:v["backup_boot_offset"] + 512]
    r.check(v["backup_boot_offset"] + 512 <= len(img), "copia del arranque fuera de la imagen")
    check_boot(r, backup, v, hidden, "copia")
    if "boot" in damage:
        r.check(not boot_ok(primary), "el arranque principal debía estar dañado")
        r.check(damage["boot"]["regions"] == [[v["offset"], 512]], "región dañada del arranque")
    else:
        check_boot(r, primary, v, hidden, "principal")
        r.check(primary == backup, "arranque principal y copia distintos")
    vol = Volume(img, v)
    mft = [vol.record_raw(i) for i in range(4)]
    for i, rec in enumerate(mft):
        r.check(rec is not None and rec[:4] == b"FILE" and struct.unpack_from("<I", rec, 0x2C)[0] == i,
                f"$MFT registro {i}")
    at = v["offset"] + v["mftmirr_lcn"] * v["cluster_size"]
    mirror = img[at:at + 4 * v["mft_record_size"]]
    if "mftmirr" in damage:
        r.check(mirror != b"".join(x or b"" for x in mft), "$MFTMirr debía diferir de $MFT")
        r.check(mirror[:4] != b"FILE", "$MFTMirr dañado conserva la firma FILE")
    else:
        r.check(mirror == b"".join(x or b"" for x in mft), "$MFTMirr distinto de los registros 0 a 3 de $MFT")
    if "mft" in damage:
        lost = sorted(f["mft_record"] for f in m["files"] if f.get("mft_damaged"))
        r.check(lost == sorted(damage["mft"]["records"]), f"registros dañados {lost}")
    check_files(r, vol, m)
    return r


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__, file=sys.stderr)
        return 2
    images, manifests = Path(sys.argv[1]), Path(sys.argv[2])
    paths = sorted(manifests.glob("*.manifest.toml"))
    if not paths:
        print(f"ERROR no hay manifiestos en {manifests}")
        return 1
    total_err = 0
    for mp in paths:
        m = tomllib.loads(mp.read_text(encoding="utf-8"))
        r = validate(images / m["file"], m)
        extra = f", {r.skipped} con lista de atributos sin comprobar" if r.skipped else ""
        print(f"{'OK' if not r.errors else 'ERROR'} {m['name']}: {r.checks} comprobaciones{extra}")
        for e in r.errors[:40]:
            print(f"  {e}")
        total_err += len(r.errors)
    print(f"{len(paths)} manifiestos, {total_err} discrepancias")
    return 1 if total_err else 0


if __name__ == "__main__":
    sys.exit(main())
