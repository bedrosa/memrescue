/*
 * memrescue_core.h: Interfaz C del motor de MemRescue. ABI versión 2.
 *
 * Reglas (ver docs/API_FFI.md):
 *  - Todas las funciones devuelven un mr_status (int32_t); 0 = MR_OK.
 *  - Cadenas de entrada: UTF-8 con puntero + longitud en bytes (no requieren terminador),
 *    longitud máxima MR_MAX_STRING_BYTES.
 *  - Cadenas de salida: el llamador aporta (buf, cap); el motor escribe UTF-8 terminado en 0
 *    y devuelve en *out_len la longitud SIN el terminador. Si no cabe: MR_ERR_BUFFER_TOO_SMALL
 *    y *out_len = longitud necesaria.
 *  - Búferes binarios de salida (mr_source_read): el llamador aporta (buf, cap); *out_len = bytes escritos.
 *  - El motor nunca devuelve memoria que el llamador deba liberar.
 *  - Orígenes y tareas se identifican con un handle uint64_t (0 nunca es válido) guardado en un
 *    registro del motor: un handle inválido o ya liberado devuelve MR_ERR_INVALID_HANDLE.
 *  - Tras un error, mr_last_error() devuelve el mensaje del último error del hilo que llamó.
 *
 * Historial: ABI 1 (M0) → ABI 2 (M1): orígenes, lectura de sectores, verificación, detección de
 * formato, códigos 10–11; mr_image_info_get rechaza contenedores no soportados.
 */
#ifndef MEMRESCUE_CORE_H
#define MEMRESCUE_CORE_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

#define MR_ABI_VERSION 2u
#define MR_MAX_STRING_BYTES 65536u
#define MR_MAX_READ_BYTES (64u * 1024u * 1024u)

typedef int32_t mr_status;
#define MR_OK                      0
#define MR_ERR_INVALID_ARGUMENT    1
#define MR_ERR_IO                  2
#define MR_ERR_PATH_REJECTED       3
#define MR_ERR_STORE               4
#define MR_ERR_CANCELLED           5
#define MR_ERR_NOT_FINISHED        6
#define MR_ERR_INVALID_HANDLE      7
#define MR_ERR_BUFFER_TOO_SMALL    8
#define MR_ERR_TASK_FAILED         9
#define MR_ERR_UNSUPPORTED_FORMAT 10
#define MR_ERR_OUT_OF_RANGE       11
#define MR_ERR_PANIC              99

/* Algoritmos de hash (máscara de bits para *_hash_start, valor único para mr_hash_result). */
#define MR_HASH_CRC32   1u
#define MR_HASH_MD5     2u
#define MR_HASH_SHA1    4u
#define MR_HASH_SHA256  8u

/* Estado de tarea. */
#define MR_TASK_RUNNING    1u
#define MR_TASK_COMPLETED  2u
#define MR_TASK_FAILED     3u
#define MR_TASK_CANCELLED  4u

/* Formatos de fichero (mr_image_probe). */
#define MR_FORMAT_RAW              1u
#define MR_FORMAT_VHD              2u
#define MR_FORMAT_VHDX             3u
#define MR_FORMAT_VMDK             4u
#define MR_FORMAT_VMDK_DESCRIPTOR  5u
#define MR_FORMAT_EWF              6u
#define MR_FORMAT_QCOW             7u
#define MR_FORMAT_VDI              8u
#define MR_FORMAT_DMG              9u

/* Opciones de mr_source_open. */
#define MR_OPEN_DEFAULT      0u
#define MR_OPEN_FORCE_RAW    1u   /* interpretar como RAW aunque parezca un contenedor */

typedef struct mr_image_info {   /* 32 bytes */
    uint64_t size_bytes;
    uint64_t sector_count;
    uint64_t trailing_bytes;
    uint32_t sector_size;
    uint32_t reserved;
} mr_image_info;

typedef struct mr_task_status {  /* 24 bytes */
    uint32_t state;
    uint32_t reserved;
    uint64_t done;               /* bytes (hash) o sectores (verificación) */
    uint64_t total;
} mr_task_status;

typedef struct mr_verify_report { /* 48 bytes */
    uint64_t sectors_requested;
    uint64_t sectors_ok;
    uint64_t sectors_bad;
    uint64_t retry_reads;
    uint64_t bad_range_count;    /* rangos disponibles con mr_verify_bad_range */
    uint32_t bad_ranges_truncated;
    uint32_t reserved;
} mr_verify_report;

uint32_t  mr_abi_version(void);
mr_status mr_engine_version(uint8_t *buf, size_t cap, size_t *out_len);
mr_status mr_last_error(uint8_t *buf, size_t cap, size_t *out_len);

/* Ficheros sin abrir como origen */
mr_status mr_image_probe(const uint8_t *path, size_t path_len, uint32_t *out_format);
mr_status mr_image_info_get(const uint8_t *path, size_t path_len, uint32_t sector_size,
                            mr_image_info *out);

/* Orígenes (imágenes abiertas en solo lectura) */
mr_status mr_source_open(const uint8_t *path, size_t path_len, uint32_t sector_size,
                         uint32_t flags, uint64_t *out_source);
mr_status mr_source_info(uint64_t source, mr_image_info *out);
mr_status mr_source_read(uint64_t source, uint64_t lba, uint32_t count,
                         uint8_t *buf, size_t cap, size_t *out_len);
mr_status mr_source_close(uint64_t source);

/* Tareas. store_path puede ser NULL (store_len = 0): sin registro en base de datos. */
mr_status mr_hash_start(const uint8_t *path, size_t path_len, uint32_t sector_size,
                        uint32_t algorithms_mask,
                        const uint8_t *store_path, size_t store_len,
                        uint64_t *out_task);
mr_status mr_source_hash_start(uint64_t source, uint32_t algorithms_mask,
                               const uint8_t *store_path, size_t store_len, uint64_t *out_task);
mr_status mr_hash_result(uint64_t task, uint32_t algorithm,
                         uint8_t *buf, size_t cap, size_t *out_len);

/* count = 0: hasta el último sector. */
mr_status mr_source_verify_start(uint64_t source, uint64_t start_lba, uint64_t count,
                                 uint32_t retries, uint64_t *out_task);
mr_status mr_verify_result(uint64_t task, mr_verify_report *out);
mr_status mr_verify_bad_range(uint64_t task, uint64_t index, uint64_t *out_lba, uint64_t *out_count);

mr_status mr_task_status_get(uint64_t task, mr_task_status *out);
mr_status mr_task_cancel(uint64_t task);
mr_status mr_task_free(uint64_t task);

#ifdef __cplusplus
}
#endif
#endif /* MEMRESCUE_CORE_H */
