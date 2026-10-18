#!/usr/bin/env python3
"""Descarga el corpus NTFS publicado por el workflow `ntfs-corpus.yml` y lo verifica. Solo para desarrollo.

Uso:
    python -I -X utf8 tools/fetch_ntfs_corpus.py [--run-id N] [--branch RAMA] [--dest DIR]

  --run-id N    ejecución concreta del workflow (debe haber terminado con éxito).
  --branch RAMA rama de la que tomar la última ejecución con éxito (por defecto `main`; si no hay
                ninguna, se usa la última con éxito de cualquier rama: el SHA-256 decide).
  --dest DIR    destino (por defecto tests/corpus/ntfs/generated, ignorado por git).

Necesita la CLI `gh` autenticada (los artefactos de Actions se descargan con la API). No instala nada,
no ejecuta nada de lo descargado y no requiere administrador.

Verificación: el artefacto se descarga en un directorio temporal nuevo y vacío. Cada imagen que
nombran los manifiestos versionados (tests/corpus/ntfs/manifests/*.manifest.toml) debe existir y tener
el tamaño y el SHA-256 de su manifiesto; `SHA256SUMS` del artefacto debe coincidir. Solo si todo
coincide se copian las imágenes, `SHA256SUMS` y `PROVENANCE.txt` al destino. Si los manifiestos del
artefacto difieren de los versionados se avisa (las imágenes valen igual si su SHA-256 coincide).
Después conviene ejecutar el validador estructural:
    python -I -X utf8 tests/generators/validate_ntfs_images.py tests/corpus/ntfs/generated tests/corpus/ntfs/manifests
"""
from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
WORKFLOW = "ntfs-corpus.yml"
WORKFLOW_NAME = "NTFS corpus"
ARTIFACT = "ntfs-corpus"
MANIFESTS = ROOT / "tests" / "corpus" / "ntfs" / "manifests"
DEFAULT_DEST = ROOT / "tests" / "corpus" / "ntfs" / "generated"


def gh(args: list[str]) -> str:
    r = subprocess.run(["gh", *args], cwd=ROOT, capture_output=True, text=True, encoding="utf-8")
    if r.returncode != 0:
        raise SystemExit(f"ERROR gh {' '.join(args)}: {r.stderr.strip()}")
    return r.stdout


def find_run(run_id: int | None, branch: str) -> dict:
    fields = "databaseId,headBranch,headSha,conclusion,createdAt,workflowName,event"
    if run_id is not None:
        run = json.loads(gh(["run", "view", str(run_id), "--json", fields]))
        if run.get("workflowName") != WORKFLOW_NAME:
            raise SystemExit(f"ERROR la ejecución {run_id} es de «{run.get('workflowName')}», no de «{WORKFLOW_NAME}»")
        if run.get("conclusion") != "success":
            raise SystemExit(f"ERROR la ejecución {run_id} no terminó con éxito ({run.get('conclusion')})")
        return run
    for b in (branch, None):
        args = ["run", "list", "--workflow", WORKFLOW, "--status", "success", "--limit", "1", "--json", fields]
        if b:
            args += ["--branch", b]
        runs = json.loads(gh(args))
        if runs:
            if b is None:
                print(f"AVISO no hay ejecuciones con éxito en «{branch}»; se usa la última de cualquier rama")
            return runs[0]
    raise SystemExit(f"ERROR no hay ejecuciones con éxito de {WORKFLOW} (lánzelo con: gh workflow run {WORKFLOW} --ref main)")


def sha256_file(p: Path) -> str:
    h = hashlib.sha256()
    with p.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def main() -> int:
    ap = argparse.ArgumentParser(description="Descarga y verifica el corpus NTFS del workflow ntfs-corpus.yml")
    ap.add_argument("--run-id", type=int)
    ap.add_argument("--branch", default="main")
    ap.add_argument("--dest", type=Path, default=DEFAULT_DEST)
    a = ap.parse_args()

    manifests = {p.name: tomllib.loads(p.read_text(encoding="utf-8")) for p in sorted(MANIFESTS.glob("*.manifest.toml"))}
    if not manifests:
        raise SystemExit(f"ERROR no hay manifiestos en {MANIFESTS}")
    run = find_run(a.run_id, a.branch)
    print(f"Ejecución {run['databaseId']} ({run.get('headBranch')}, {run.get('headSha', '')[:12]}, {run.get('createdAt')})")

    dest = a.dest.resolve()
    dest.parent.mkdir(parents=True, exist_ok=True)
    problems: list[str] = []
    with tempfile.TemporaryDirectory(prefix="ntfs-corpus-", dir=dest.parent) as tmp:
        dl = Path(tmp) / "artifact"
        dl.mkdir()
        gh(["run", "download", str(run["databaseId"]), "--name", ARTIFACT, "--dir", str(dl)])
        sums = {}
        sums_file = dl / "SHA256SUMS"
        if sums_file.is_file():
            for line in sums_file.read_text(encoding="utf-8").splitlines():
                if line.strip():
                    digest, name = line.split(maxsplit=1)
                    sums[name.lstrip("*")] = digest
        else:
            problems.append("el artefacto no tiene SHA256SUMS")
        images = []
        for mname, m in manifests.items():
            img = dl / m["file"]
            if not img.is_file():
                problems.append(f"{m['file']}: no está en el artefacto")
                continue
            digest = sha256_file(img)
            size = img.stat().st_size
            ok = digest == m["image_sha256"] and size == m["image_size"]
            if not ok:
                problems.append(f"{m['file']}: SHA-256 {digest} y tamaño {size}; el manifiesto dice "
                                f"{m['image_sha256']} y {m['image_size']}")
            if sums.get(m["file"]) != digest:
                problems.append(f"{m['file']}: no coincide con SHA256SUMS del artefacto")
            print(f"{'OK   ' if ok else 'ERROR'} {m['file']}  {digest}")
            images.append(img)
            art_manifest = dl / "manifests" / mname
            if art_manifest.is_file() and art_manifest.read_bytes() != (MANIFESTS / mname).read_bytes():
                print(f"AVISO {mname}: el manifiesto del artefacto difiere del versionado")
        if problems:
            for p in problems:
                print(f"ERROR {p}")
            print("No se ha copiado nada al destino.")
            return 1
        dest.mkdir(parents=True, exist_ok=True)
        for old in dest.glob("*.img"):
            old.unlink()
        for img in images:
            shutil.move(str(img), dest / img.name)
        for extra in ("SHA256SUMS", "PROVENANCE.txt"):
            if (dl / extra).is_file():
                shutil.copyfile(dl / extra, dest / extra)
    print(f"{len(images)} imágenes verificadas en {dest}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
