#!/usr/bin/env python3
"""Vuelve a comprobar los manifiestos NTFS versionados contra The Sleuth Kit (en el ejecutor Linux).

Uso:
    python3 -I verify_tsk.py <dir_imagenes> <dir_manifiestos>

Es independiente de build_manifests.py (no lo importa ni importa el generador) y usa otras vías de TSK
siempre que puede: `ifind -n` (búsqueda por nombre en los índices de directorio) y `ffind` (nombre a
partir del registro) para las rutas vivas, `fls -r -p -u` para el conjunto exacto de entradas vivas,
`istat` para estado, secuencia, residencia, runs y dispersos, `icat` para el SHA-256 de cada flujo y
`mmls`/`fsstat` para la partición y la geometría del volumen.

Código de salida 0 si todo coincide; 1 si hay discrepancias.
"""
from __future__ import annotations

import hashlib
import re
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path


def tsk(args: list[str]) -> subprocess.CompletedProcess:
    return subprocess.run(args, capture_output=True)


def text(r: subprocess.CompletedProcess) -> str:
    return r.stdout.decode("utf-8", "replace")


def data_attrs(istat_out: str) -> list[dict]:
    """Atributos $DATA de la salida de `istat -r`: nombre, residencia, tamaño y runs en bruto."""
    attrs, cur = [], None
    for line in istat_out.splitlines():
        m = re.match(r"^Type:\s+(\$\S+)\s+\((\d+)-(\d+)\)\s+Name:\s+(.*?)\s+(Non-Resident|Resident).*?size:\s+(\d+)", line)
        if m:
            cur = None
            if m.group(1) == "$DATA":
                cur = {"name": "" if m.group(4) == "N/A" else m.group(4), "id": int(m.group(3)),
                       "resident": m.group(5) == "Resident", "size": int(m.group(6)), "pieces": []}
                attrs.append(cur)
            continue
        if cur is not None:
            m = re.search(r"Starting address:\s*(\S+?),\s*length:\s*(\d+)(.*)$", line)
            if m:
                hole = "sparse" in m.group(3).lower() or not m.group(1).isdigit()
                cur["pieces"].append((None if hole else int(m.group(1)), int(m.group(2))))
    return attrs


def extents(pieces: list[tuple[int | None, int]]) -> tuple[list[list[int]], list[list[int]]]:
    out: list[list] = []
    vcn = 0
    for lcn, n in pieces:
        if out and ((out[-1][1] is None and lcn is None) or
                    (out[-1][1] is not None and lcn is not None and out[-1][1] + out[-1][2] == lcn)):
            out[-1][2] += n
        else:
            out.append([vcn, lcn, n])
        vcn += n
    return [[v, l, n] for v, l, n in out if l is not None], [[v, n] for v, l, n in out if l is None]


def check_stream(errs: list[str], where: str, image: Path, off: int, rec: int, attr: dict, want: dict,
                 expect_sha: str) -> None:
    for k in ("size", "resident"):
        if attr[k] != want[k]:
            errs.append(f"{where}: {k} TSK={attr[k]} manifiesto={want[k]}")
    if not attr["resident"]:
        runs, sparse = extents(attr["pieces"])
        if runs != want.get("runs", []) or sparse != want.get("sparse", []):
            errs.append(f"{where}: runs TSK={runs}/{sparse} manifiesto={want.get('runs')}/{want.get('sparse')}")
    r = tsk(["icat", "-f", "ntfs", "-o", str(off), str(image), f"{rec}-128-{attr['id']}"])
    if r.returncode != 0 or hashlib.sha256(r.stdout).hexdigest() != expect_sha:
        errs.append(f"{where}: SHA-256 de icat distinto del manifiesto")


def verify(image: Path, m: dict) -> list[str]:
    errs: list[str] = []
    h = hashlib.sha256(image.read_bytes()).hexdigest()
    if h != m["image_sha256"]:
        errs.append(f"SHA-256 de la imagen {h} != {m['image_sha256']}")
    vol = m["volume"]
    off = vol["offset"] // 512
    if "partition" in m:
        rows = [ln for ln in text(tsk(["mmls", str(image)])).splitlines()
                if re.match(r"^\s*\d+:\s+\d+(?::\d+)?\s+\d+", ln)]
        p = m["partition"]
        if len(rows) != 1 or [int(x) for x in rows[0].split()[2:5:2]] != [p["start_lba"], p["sectors"]]:
            errs.append(f"mmls no ve la partición del manifiesto: {rows}")
    damage = {d["kind"]: d for d in m.get("damage", [])}
    with tempfile.TemporaryDirectory() as tmp:
        src = image
        if "boot" in damage:
            if tsk(["fsstat", "-f", "ntfs", "-o", str(off), str(image)]).returncode == 0:
                errs.append("TSK abre una imagen con el arranque principal dañado")
            data = bytearray(image.read_bytes())
            b = vol["backup_boot_offset"]
            data[vol["offset"]:vol["offset"] + 512] = data[b:b + 512]
            src = Path(tmp) / image.name
            src.write_bytes(bytes(data))
        fs = text(tsk(["fsstat", "-f", "ntfs", "-o", str(off), str(src)]))
        for label, key in (("Volume Serial Number", "serial"), ("First Cluster of MFT", "mft_lcn"),
                           ("First Cluster of MFT Mirror", "mftmirr_lcn"), ("Cluster Size", "cluster_size"),
                           ("Volume Name", "label")):
            mm = re.search(rf"^{label}:\s*(.*)$", fs, re.M)
            if not mm or mm.group(1).strip().lower() != str(vol[key]).lower():
                errs.append(f"fsstat {label}: {mm.group(1).strip() if mm else None} != {vol[key]}")
        live_tsk = set()
        for line in text(tsk(["fls", "-f", "ntfs", "-o", str(off), "-r", "-p", "-u", str(src)])).splitlines():
            mm = re.match(r"^\S+\s+(?:\*\s+)?[\d-]+(?:\(realloc\))?:\s+(.*)$", line)
            if not mm:
                continue
            name = mm.group(1)
            if name.startswith("$") or ":" in name or name in (".", ".."):
                continue
            live_tsk.add("/" + name)
        live_man = {f["path"] for f in m["files"] if f["state"] == "live"}
        if live_tsk != live_man:
            errs.append(f"fls -u: sobran {sorted(live_tsk - live_man)}, faltan {sorted(live_man - live_tsk)}")
        for f in m["files"]:
            where, rec = f["path"], f["mft_record"]
            ist = tsk(["istat", "-f", "ntfs", "-o", str(off), "-r", str(src), str(rec)])
            if f.get("mft_damaged"):
                if ist.returncode == 0:
                    errs.append(f"{where}: el registro {rec} debía estar dañado y TSK lo lee")
                continue
            if ist.returncode != 0:
                errs.append(f"{where}: istat {rec} falla")
                continue
            out = text(ist)
            state = re.search(r"^(Not )?Allocated (File|Directory)\s*$", out, re.M)
            allocated = state is not None and state.group(1) is None
            if allocated != (f["state"] == "live"):
                errs.append(f"{where}: asignado={allocated}, estado={f['state']}")
            mm = re.search(r"^Entry:\s*(\d+)\s+Sequence:\s*(\d+)", out, re.M)
            if not mm or int(mm.group(2)) != f["sequence"]:
                errs.append(f"{where}: secuencia distinta")
            if f["state"] == "live":
                r = tsk(["ifind", "-f", "ntfs", "-o", str(off), "-n", f["path"], str(src)])
                got = re.match(r"\s*(\d+)", text(r))
                if not got or int(got.group(1)) != rec:
                    errs.append(f"{where}: ifind -n da {text(r).strip()!r}, manifiesto {rec}")
                r = tsk(["ffind", "-f", "ntfs", "-o", str(off), str(src), str(rec)])
                if f["path"] not in [ln.strip() for ln in text(r).splitlines()]:
                    errs.append(f"{where}: ffind da {text(r).strip()!r}")
            attrs = data_attrs(out)
            main = [a for a in attrs if not a["name"]]
            if f["type"] == "file":
                if len(main) != 1:
                    errs.append(f"{where}: {len(main)} $DATA sin nombre")
                else:
                    sha = f["sha256"] if f["recoverable"] == "full" else f["recovered_sha256"]
                    check_stream(errs, where, src, off, rec, main[0], f, sha)
            named = {a["name"]: a for a in attrs if a["name"]}
            want_ads = {a["name"]: a for a in f.get("ads", [])}
            if set(named) != set(want_ads):
                errs.append(f"{where}: flujos TSK {sorted(named)} manifiesto {sorted(want_ads)}")
            for name in sorted(set(named) & set(want_ads)):
                check_stream(errs, f"{where}:{name}", src, off, rec, named[name], want_ads[name], want_ads[name]["sha256"])
    return errs


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__, file=sys.stderr)
        return 2
    images, manifests = Path(sys.argv[1]), Path(sys.argv[2])
    paths = sorted(manifests.glob("*.manifest.toml"))
    if not paths:
        print("ERROR no hay manifiestos")
        return 1
    failed = 0
    for mp in paths:
        m = tomllib.loads(mp.read_text(encoding="utf-8"))
        errs = verify(images / m["file"], m)
        status = "OK" if not errs else "ERROR"
        print(f"{status} {m['name']}: {len(m['files'])} entradas")
        for e in errs:
            print(f"  {e}")
        failed += bool(errs)
    print(f"{len(paths)} manifiestos, {failed} con discrepancias")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
