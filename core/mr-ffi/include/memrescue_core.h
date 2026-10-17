/*
 * memrescue_core.h: Interfaz C del motor de MemRescue. ABI versión 1.
 *
 * Reglas (ver docs/API_FFI.md):
 *  - Todas las funciones devuelven un mr_status (int32_t); 0 = MR_OK.
 *  - Cadenas de entrada: UTF-8 con puntero + longitud en bytes (no requieren terminador).
 *  - Cadenas de salida: el llamador aporta (buf, cap); el motor escribe UTF-8 terminado en 0
 *    y devuelve en *out_len la longitud SIN el terminador. Si no cabe: MR_ERR_BUFFER_TOO_SMALL
 *    y *out_len = longitud necesaria.
 *  - El motor nunca devuelve memoria que el llamador deba liberar.
 *  - Las tareas se identifican con un handle uint64_t (0 nunca es válido). Se liberan con
 *    mr_task_free. Un handle inválido devuelve MR_ERR_INVALID_HANDLE.
 *  - Tras un error, mr_last_error() devuelve el mensaje del último error del hilo que llamó.
 */
#ifndef MEMRESCUE_CORE_H
#define MEMRESCUE_CORE_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

#define MR_ABI_VERSION 1u

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
#define MR_ERR_PANIC              99

/* Algoritmos de hash (máscara de bits para mr_hash_start, valor único para mr_hash_result). */
#define MR_HASH_CRC32   1u
#define MR_HASH_MD5     2u
#define MR_HASH_SHA1    4u
#define MR_HASH_SHA256  8u

/* Estado de tarea. */
#define MR_TASK_RUNNING    1u
#define MR_TASK_COMPLETED  2u
#define MR_TASK_FAILED     3u
#define MR_TASK_CANCELLED  4u

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
    uint64_t done;
    uint64_t total;
} mr_task_status;

uint32_t  mr_abi_version(void);
mr_status mr_engine_version(uint8_t *buf, size_t cap, size_t *out_len);
mr_status mr_last_error(uint8_t *buf, size_t cap, size_t *out_len);

mr_status mr_image_info_get(const uint8_t *path, size_t path_len, uint32_t sector_size,
                            mr_image_info *out);

/* store_path puede ser NULL (store_len = 0): sin registro en base de datos. */
mr_status mr_hash_start(const uint8_t *path, size_t path_len, uint32_t sector_size,
                        uint32_t algorithms_mask,
                        const uint8_t *store_path, size_t store_len,
                        uint64_t *out_task);
mr_status mr_hash_result(uint64_t task, uint32_t algorithm,
                         uint8_t *buf, size_t cap, size_t *out_len);

mr_status mr_task_status_get(uint64_t task, mr_task_status *out);
mr_status mr_task_cancel(uint64_t task);
mr_status mr_task_free(uint64_t task);

#ifdef __cplusplus
}
#endif
#endif /* MEMRESCUE_CORE_H */
