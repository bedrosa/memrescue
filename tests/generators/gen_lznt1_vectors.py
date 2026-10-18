#!/usr/bin/env python3
"""Vectores de prueba LZNT1 generados con el compresor de Windows (oráculo independiente).

Uso (solo Windows; no requiere administrador):
    python -I -X utf8 tests/generators/gen_lznt1_vectors.py tests/vectors/lznt1

El descompresor LZNT1 de `mr-fs-ntfs` es una implementación original de la especificación pública
[MS-XCA] §2.5. Para comprobarlo con algo que no comparta código con él, este script llama por
ctypes a `RtlCompressBuffer` y `RtlDecompressBuffer` de `ntdll.dll` (COMPRESSION_FORMAT_LZNT1) con
entradas deterministas (generador xorshift propio con semilla fija, entropía variada y tamaños de
1 byte a varias unidades de compresión de 64 KiB; una entrada toda a ceros no sirve: Windows
responde STATUS_BUFFER_ALL_ZEROS y NTFS la guarda dispersa) y guarda:

- `<nombre>.hex`: los bytes comprimidos por Windows, en hexadecimal (64 bytes por línea; el
  repositorio no admite binarios sin regenerador multiplataforma).
- `manifest.toml`: por vector, el tamaño y el SHA-256 de los datos sin comprimir y de los
  comprimidos, el motor de compresión y el resultado esperado.

Además hay vectores construidos a mano (`kind = "crafted"`) que fijan el comportamiento de
`RtlDecompressBuffer` en casos límite: bloques cortos seguidos de otro (se rellenan con ceros hasta
4096), firma de cabecera distinta de 3 (se acepta), byte final suelto, cabecera 0 seguida de basura,
bloque truncado y referencia anterior al inicio del bloque (`expect = "error"`).

Antes de escribir nada se comprueba que `RtlDecompressBuffer` devuelve exactamente la entrada
original de cada vector comprimido. Otra versión de Windows puede comprimir con otros bytes (los
datos sin comprimir y su SHA-256 no cambian): por eso se versionan los vectores y la CI no necesita
Windows. Las pruebas de Rust (`core/mr-fs-ntfs/src/lznt1.rs`) leen estos
ficheros, descomprimen y comparan tamaño y SHA-256 con el manifiesto.
"""
from __future__ import annotations

import ctypes
import hashlib
import struct
import sys
from ctypes import wintypes
from pathlib import Path

FORMAT_LZNT1 = 0x0002
ENGINE_STANDARD = 0x0000
ENGINE_MAXIMUM = 0x0100
STATUS_SUCCESS = 0
CHUNK = 4096


def xorshift(seed: int):
    s = seed or 1
    while True:
        s ^= (s >> 12)
        s ^= (s << 25) & 0xFFFFFFFFFFFFFFFF
        s ^= (s >> 27)
        yield (s * 0x2545F4914F6CDD1D) & 0xFFFFFFFFFFFFFFFF


def rand_bytes(seed: int, n: int, alphabet: int = 256) -> bytes:
    g = xorshift(seed)
    return bytes((next(g) >> 24) % alphabet for _ in range(n))


WORDS = [b"sector", b"cluster", b"registro", b"atributo", b"flujo", b"disperso", b"volumen",
         b"datos", b"copia", b"tabla", b"indice", b"nombre", b" ", b"\r\n", b", ", b"0123"]


def text(seed: int, n: int) -> bytes:
    g = xorshift(seed)
    out = bytearray()
    while len(out) < n:
        out += WORDS[next(g) % len(WORDS)]
    return bytes(out[:n])


def runs_of_bytes(seed: int, n: int) -> bytes:
    g = xorshift(seed)
    out = bytearray()
    while len(out) < n:
        v = next(g)
        out += bytes([v & 0xFF]) * (1 + (v >> 8) % 600)
    return bytes(out[:n])


def mixed(seed: int, n: int) -> bytes:
    g = xorshift(seed)
    out = bytearray()
    i = 0
    while len(out) < n:
        k = 1 + next(g) % 9000
        part = (rand_bytes, text, runs_of_bytes)[i % 3]
        out += part(seed + i, k) if part is not rand_bytes else rand_bytes(seed + i, k, 16)
        i += 1
    return bytes(out[:n])


# (nombre, datos, motor)
def cases() -> list[tuple[str, bytes, int]]:
    return [
        ("one_byte", b"Z", ENGINE_STANDARD),
        ("three_bytes", b"abc", ENGINE_STANDARD),
        ("zeros_64k_one_byte", bytes(40000) + b"" + bytes(25535), ENGINE_STANDARD),
        ("zeros_64k_one_byte_max", bytes(40000) + b"" + bytes(25535), ENGINE_MAXIMUM),
        ("period3_12k", b"abc" * 4096, ENGINE_STANDARD),
        ("text_4095", text(1, 4095), ENGINE_STANDARD),
        ("text_4096", text(2, 4096), ENGINE_STANDARD),
        ("text_4097", text(3, 4097), ENGINE_STANDARD),
        ("text_64k", text(4, 65536), ENGINE_STANDARD),
        ("text_200k_max", text(5, 200_000), ENGINE_MAXIMUM),
        ("alphabet4_50000", rand_bytes(6, 50_000, 4), ENGINE_STANDARD),
        ("alphabet16_64k_max", rand_bytes(7, 65536, 16), ENGINE_MAXIMUM),
        ("random_4096", rand_bytes(8, 4096), ENGINE_STANDARD),
        ("random_9000", rand_bytes(9, 9000), ENGINE_STANDARD),
        ("byte_runs_128k", runs_of_bytes(10, 131_072), ENGINE_STANDARD),
        ("mixed_3cu_plus", mixed(11, 3 * 65536 + 123), ENGINE_STANDARD),
        ("mixed_3cu_plus_max", mixed(11, 3 * 65536 + 123), ENGINE_MAXIMUM),
    ]


def hdr(flag_compressed: bool, data_len: int, sig: int = 3) -> bytes:
    return struct.pack("<H", (0x8000 if flag_compressed else 0) | (sig << 12) | (data_len - 1))


# (nombre, comprimido, tamaño del búfer de salida)
def crafted() -> list[tuple[str, bytes, int]]:
    return [
        ("crafted_short_raw_then_raw", hdr(False, 10) + b"0123456789" + hdr(False, 5) + b"ABCDE", 16384),
        ("crafted_short_compressed_then_raw", hdr(True, 4) + bytes([0]) + b"xyz" + hdr(False, 2) + b"QQ", 16384),
        ("crafted_signature_0", hdr(False, 5, sig=0) + b"ABCDE", 64),
        ("crafted_trailing_byte", hdr(False, 5) + b"ABCDE" + b"\x07", 64),
        ("crafted_end_then_garbage", hdr(False, 5) + b"ABCDE" + b"\x00\x00XYZ", 64),
        ("crafted_truncated_chunk", hdr(False, 10) + b"0123", 64),
        ("crafted_backref_at_start", hdr(True, 3) + bytes([1, 0, 0]), 64),
        ("crafted_backref_too_far", hdr(True, 4) + bytes([0b10, ord("a"), 0x00, 0x10]), 64),
        ("crafted_overlapping_copy", hdr(True, 4) + bytes([0b10, ord("q"), 0x05, 0x00]), 64),
    ]


class Ntdll:
    def __init__(self) -> None:
        self.nt = ctypes.WinDLL("ntdll")

    def compress(self, data: bytes, engine: int) -> bytes:
        ws, fws = wintypes.ULONG(), wintypes.ULONG()
        st = self.nt.RtlGetCompressionWorkSpaceSize(wintypes.USHORT(FORMAT_LZNT1 | engine),
                                                    ctypes.byref(ws), ctypes.byref(fws))
        if st & 0xFFFFFFFF:
            raise RuntimeError(f"RtlGetCompressionWorkSpaceSize: 0x{st & 0xFFFFFFFF:08X}")
        work = ctypes.create_string_buffer(ws.value)
        out = ctypes.create_string_buffer(len(data) + len(data) // 8 + 2 * CHUNK)
        final = wintypes.ULONG()
        st = self.nt.RtlCompressBuffer(wintypes.USHORT(FORMAT_LZNT1 | engine), ctypes.c_char_p(data),
                                       wintypes.ULONG(len(data)), out, wintypes.ULONG(len(out)),
                                       wintypes.ULONG(CHUNK), ctypes.byref(final), work)
        if st & 0xFFFFFFFF:
            raise RuntimeError(f"RtlCompressBuffer: 0x{st & 0xFFFFFFFF:08X}")
        return out.raw[:final.value]

    def decompress(self, comp: bytes, out_size: int) -> tuple[int, bytes]:
        out = ctypes.create_string_buffer(max(out_size, 1))
        final = wintypes.ULONG()
        st = self.nt.RtlDecompressBuffer(wintypes.USHORT(FORMAT_LZNT1), out, wintypes.ULONG(out_size),
                                         ctypes.c_char_p(comp), wintypes.ULONG(len(comp)), ctypes.byref(final))
        return st & 0xFFFFFFFF, out.raw[:final.value]


def write_hex(path: Path, data: bytes) -> None:
    lines = [data[i:i + 64].hex() for i in range(0, len(data), 64)]
    path.write_text("\n".join(lines) + "\n", encoding="ascii", newline="\n")


def sha(b: bytes) -> str:
    return hashlib.sha256(b).hexdigest()


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__)
        return 2
    if sys.platform != "win32":
        print("este generador necesita ntdll.dll (Windows)")
        return 2
    out_dir = Path(sys.argv[1])
    nt = Ntdll()
    entries: list[str] = []
    files: dict[str, bytes] = {}
    for name, plain, engine in cases():
        comp = nt.compress(plain, engine)
        st, back = nt.decompress(comp, len(plain))
        if st != STATUS_SUCCESS or back != plain:
            raise RuntimeError(f"{name}: RtlDecompressBuffer no devuelve la entrada (0x{st:08X})")
        files[name] = comp
        entries.append("\n".join([
            "[[vector]]",
            f'name = "{name}"',
            'kind = "compressed"',
            f'engine = "{"maximum" if engine == ENGINE_MAXIMUM else "standard"}"',
            'expect = "ok"',
            f"plain_size = {len(plain)}",
            f'plain_sha256 = "{sha(plain)}"',
            f"compressed_size = {len(comp)}",
            f'compressed_sha256 = "{sha(comp)}"',
        ]))
    for name, comp, out_size in crafted():
        st, back = nt.decompress(comp, out_size)
        files[name] = comp
        lines = [
            "[[vector]]",
            f'name = "{name}"',
            'kind = "crafted"',
            f"output_buffer = {out_size}",
            f"compressed_size = {len(comp)}",
            f'compressed_sha256 = "{sha(comp)}"',
        ]
        if st == STATUS_SUCCESS:
            lines += ['expect = "ok"', f"plain_size = {len(back)}", f'plain_sha256 = "{sha(back)}"']
        else:
            lines += ['expect = "error"', f'windows_status = "0x{st:08X}"']
        entries.append("\n".join(lines))
    out_dir.mkdir(parents=True, exist_ok=True)
    for name, comp in files.items():
        write_hex(out_dir / f"{name}.hex", comp)
    head = [
        "# Generado por tests/generators/gen_lznt1_vectors.py con RtlCompressBuffer/RtlDecompressBuffer",
        "# de ntdll.dll (COMPRESSION_FORMAT_LZNT1) como oráculo: no editar a mano.",
        "# Cada vector está en <name>.hex (bytes comprimidos en hexadecimal).",
        "schema_version = 1",
        "",
    ]
    (out_dir / "manifest.toml").write_text("\n".join(head) + "\n\n".join(entries) + "\n",
                                           encoding="utf-8", newline="\n")
    total = sum(len(c) for c in files.values())
    print(f"{len(files)} vectores ({total} bytes comprimidos) en {out_dir}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
