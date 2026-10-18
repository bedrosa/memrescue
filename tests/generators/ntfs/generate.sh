#!/usr/bin/env bash
# Genera el corpus NTFS en un ejecutor Linux desechable (GitHub Actions, workflow ntfs-corpus.yml).
#
# Uso: bash tests/generators/ntfs/generate.sh <directorio_salida>
#
# Necesita: mkntfs y ntfs-3g (paquete ntfs-3g), libfaketime (paquete libfaketime), sfdisk (util-linux),
# python3 y sudo sin contraseña (solo para el montaje FUSE de ntfs-3g). No usa dispositivos loop:
# mkntfs y ntfs-3g trabajan directamente sobre ficheros de imagen.
#
# Reproducibilidad:
#   - mkntfs -T fija todas las marcas de tiempo del formateo a 1970-01-01 y, con ellas, la semilla de
#     su generador aleatorio, de modo que el número de serie del volumen también es fijo.
#   - ntfs-3g se ejecuta con libfaketime y la hora congelada en FAKE_TIME: todos los ficheros creados
#     tienen las mismas marcas de tiempo en cada ejecución.
#   - El contenido de los ficheros es determinista (ntfs_tree.py) y las operaciones siguen un orden fijo.
#   - Las tablas MBR/GPT usan identificadores fijos (label-id, uuid).
#
# Salida: <dir>/images/*.img, <dir>/intent/*.intent.json y <dir>/SHA256SUMS.
set -euo pipefail

OUT=${1:?uso: generate.sh <directorio_salida>}
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
export PATH="$PATH:/usr/sbin:/sbin"
FAKE_TIME="2026-01-01 00:00:00"
VOLUME_BYTES=$((32 * 1024 * 1024))
PART_START=2048
FAKETIME_LIB=$(dpkg -L libfaketime | grep -E '/libfaketimeMT\.so\.1$' | head -n 1)
[ -n "$FAKETIME_LIB" ] || { echo "no se encuentra libfaketimeMT.so.1" >&2; exit 1; }

mkdir -p "$OUT/images" "$OUT/intent"
OUT=$(cd "$OUT" && pwd)
WORK=$(mktemp -d)
trap 'sudo umount "$WORK/mnt" 2>/dev/null || true; rm -rf "$WORK"' EXIT
mkdir -p "$WORK/mnt"

# make_volume <fichero> <sector_inicial_de_la_particion> <etiqueta>
make_volume() {
  rm -f "$1"
  truncate -s "$VOLUME_BYTES" "$1"
  mkntfs --force --quiet --quick --zero-time --sector-size 512 --cluster-size 4096 \
    --partition-start "$2" --heads 255 --sectors-per-track 63 --label "$3" "$1"
}

# populate <escenario> <fichero> <intencion.json>
populate() {
  local scenario=$1 image=$2 intent=$3 pid i
  sudo env LD_PRELOAD="$FAKETIME_LIB" FAKETIME="$FAKE_TIME" FAKETIME_DONT_FAKE_MONOTONIC=1 \
    ntfs-3g -o no_detach,noatime "$image" "$WORK/mnt" &
  pid=$!
  for i in $(seq 1 100); do
    mountpoint -q "$WORK/mnt" && break
    sleep 0.1
  done
  mountpoint -q "$WORK/mnt" || { echo "ntfs-3g no montó $image" >&2; exit 1; }
  sudo python3 -I "$HERE/ntfs_tree.py" "$scenario" "$WORK/mnt" "$intent"
  sudo umount "$WORK/mnt"
  wait "$pid"
  sudo chown "$(id -u):$(id -g)" "$intent"
}

# volume_image <nombre> <escenario> <etiqueta>: volumen NTFS sin tabla de particiones (empieza en el byte 0)
volume_image() {
  local img="$OUT/images/$1.img"
  make_volume "$img" 0 "$3"
  populate "$2" "$img" "$OUT/intent/$1.intent.json"
}

# wrapped_image <nombre> <mbr|gpt> <etiqueta>: disco con tabla y una partición NTFS en la LBA 2048
wrapped_image() {
  local name=$1 scheme=$2 img="$OUT/images/$1.img" vol="$WORK/$1.vol"
  local sectors=$((VOLUME_BYTES / 512)) tail=0
  make_volume "$vol" "$PART_START" "$3"
  populate basic "$vol" "$OUT/intent/$name.intent.json"
  [ "$scheme" = gpt ] && tail=2048
  rm -f "$img"
  truncate -s $(((PART_START + sectors + tail) * 512)) "$img"
  if [ "$scheme" = mbr ]; then
    printf 'label: dos\nlabel-id: 0x4d524e31\nunit: sectors\nsector-size: 512\n\nstart=%d, size=%d, type=7, bootable\n' \
      "$PART_START" "$sectors" | sfdisk --quiet --wipe never --no-reread --no-tell-kernel "$img"
  else
    printf 'label: gpt\nlabel-id: 4D52A001-0000-4000-8000-000000000001\nunit: sectors\nsector-size: 512\nfirst-lba: 2048\n\nstart=%d, size=%d, type=EBD0A0A2-B9E5-4433-87C0-68B6B72699C7, uuid=4D52A001-0000-4000-8000-0000000000A1, name="MemRescue NTFS"\n' \
      "$PART_START" "$sectors" | sfdisk --quiet --wipe never --no-reread --no-tell-kernel "$img"
  fi
  dd if="$vol" of="$img" bs=512 seek="$PART_START" conv=notrunc status=none
  rm -f "$vol"
}

# damaged_image <tipo>: copia de ntfs_basic con corrupción determinista
damaged_image() {
  local name="ntfs_damaged_$1"
  cp "$OUT/images/ntfs_basic.img" "$OUT/images/$name.img"
  python3 -I "$HERE/ntfs_damage.py" "$1" "$OUT/images/$name.img" "$OUT/intent/ntfs_basic.intent.json" \
    "$OUT/intent/$name.intent.json"
}

volume_image ntfs_basic basic MR_BASIC
volume_image ntfs_deleted deleted MR_DELETED
volume_image ntfs_overwritten overwritten MR_OVERWR
volume_image ntfs_fragmented fragmented MR_FRAG
volume_image ntfs_sparse_ads sparse_ads MR_SPARSE
damaged_image boot
damaged_image mft
damaged_image mftmirr
wrapped_image ntfs_mbr mbr MR_MBR
wrapped_image ntfs_gpt gpt MR_GPT

(cd "$OUT/images" && sha256sum -- *.img) > "$OUT/SHA256SUMS"
cat "$OUT/SHA256SUMS"
