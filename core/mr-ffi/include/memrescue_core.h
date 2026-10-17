/*
 * memrescue_core.h: Interfaz C del motor de MemRescue. ABI versión 3.
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
 * ABI 3 (M1, iteración 2): contenedores VHD/VHDX/VMDK, imágenes divididas (MR_OPEN_SPLIT,
 * mr_split_discover), rangos como origen (mr_source_open_range), lectura con estado por sector
 * (mr_source_read_mapped), verificación con salto y lectura hacia atrás e informe con todos los
 * estados (mr_verify_untrusted_range sustituye a mr_verify_bad_range), códigos 12–14,
 * sector_size = 0 (nativo) y campo format en mr_image_info.
 * Añadido sin cambiar la ABI 3: verificación extendida con política de omisión de errores
 * (mr_source_verify_start_ex, mr_verify_options, mr_verify_error_summary, mr_verify_error_code)
 * y bit MR_OPEN_TOLERANT en las opciones de mr_source_open (modo tolerante de contenedores).
 * Añadido sin cambiar la ABI 3: lectura con estado y patrones de relleno configurables
 * (mr_source_read_mapped_ex, mr_read_options). mr_source_read_mapped no cambia.
 */
#ifndef MEMRESCUE_CORE_H
#define MEMRESCUE_CORE_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

#define MR_ABI_VERSION 3u
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
#define MR_ERR_CORRUPT            12   /* estructura del contenedor dañada o incoherente */
#define MR_ERR_EXTERNAL_REFERENCE 13   /* necesita otro fichero (padre, extensión) que no se abre */
#define MR_ERR_UNSUPPORTED_FEATURE 14  /* variante del formato aún no soportada */
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

/* Opciones de mr_source_open. sector_size = 0: el nativo del formato (512 en RAW). */
#define MR_OPEN_DEFAULT      0u
#define MR_OPEN_FORCE_RAW    1u   /* interpretar como RAW aunque parezca un contenedor */
#define MR_OPEN_SPLIT        2u   /* abrir la imagen dividida que empieza en path (x.001 / x.000) */
#define MR_OPEN_TOLERANT     4u   /* VHD/VHDX/VMDK con tablas de asignación dañadas: los bloques con
                                     entradas inválidas o ambiguas se descartan (lectura con error,
                                     estado MR_SECTOR_UNREADABLE en las lecturas tolerantes) y
                                     mr_source_describe indica «modo tolerante» y cuántos se
                                     descartaron. Sin este bit se rechazan con MR_ERR_CORRUPT.
                                     Sin efecto en RAW e imágenes divididas. */

/* Estado de cada sector (mr_source_read_mapped, mr_verify_untrusted_range). */
#define MR_SECTOR_READ        0u  /* leído al primer intento */
#define MR_SECTOR_RECOVERED   1u  /* leído tras reintentos */
#define MR_SECTOR_UNREADABLE  2u  /* ilegible: el contenido es un PATRÓN DE RELLENO, no datos */
#define MR_SECTOR_SKIPPED     3u  /* no leído (salto): PATRÓN DE RELLENO, no datos */
#define MR_SECTOR_SPARSE      4u  /* no asignado en el contenedor: ceros legítimos */

/* Opciones de mr_source_verify_start (y flags de mr_verify_options). */
#define MR_VERIFY_READ_BACK  1u   /* tras un salto, leer hacia atrás hasta el siguiente error */
/* Solo mr_source_verify_start_ex: «ignorar todo», ningún error se reintenta. */
#define MR_VERIFY_IGNORE_ALL_ERRORS 2u
#define MR_MAX_SKIP_CODES  256u   /* máximo de elementos en mr_verify_options.skip_codes */

/* Opciones de mr_source_read_mapped_ex (flags de mr_read_options). */
#define MR_READ_READ_BACK    1u   /* tras un salto, leer hacia atrás hasta el siguiente error */

typedef struct mr_image_info {   /* 32 bytes */
    uint64_t size_bytes;
    uint64_t sector_count;
    uint64_t trailing_bytes;
    uint32_t sector_size;
    uint32_t format;             /* MR_FORMAT_* con el que se interpreta */
} mr_image_info;

typedef struct mr_task_status {  /* 24 bytes */
    uint32_t state;
    uint32_t reserved;
    uint64_t done;               /* bytes (hash) o sectores (verificación) */
    uint64_t total;
} mr_task_status;

typedef struct mr_verify_report { /* 72 bytes */
    uint64_t sectors_requested;
    uint64_t sectors_read;
    uint64_t sectors_recovered;
    uint64_t sectors_unreadable;
    uint64_t sectors_skipped;
    uint64_t sectors_sparse;
    uint64_t retry_reads;
    uint64_t untrusted_range_count; /* tramos ilegibles o saltados (mr_verify_untrusted_range) */
    uint32_t ranges_truncated;      /* 1 = lista de tramos truncada; los recuentos son exactos */
    uint32_t reserved;
} mr_verify_report;

/* Opciones de mr_source_verify_start_ex (48 bytes en x64). struct_size = sizeof(mr_verify_options):
 * permite ampliar la estructura sin romper la ABI; un tamaño desconocido se rechaza.
 * Política de errores: un sector cuya lectura individual falla con un código de skip_codes (o con
 * cualquier error si MR_VERIFY_IGNORE_ALL_ERRORS) no se reintenta y queda MR_SECTOR_UNREADABLE
 * (patrón de relleno, nunca datos). El salto y la lectura hacia atrás se aplican igual. */
typedef struct mr_verify_options {
    uint32_t struct_size;
    uint32_t flags;              /* MR_VERIFY_READ_BACK | MR_VERIFY_IGNORE_ALL_ERRORS */
    uint64_t start_lba;
    uint64_t count;              /* 0 = hasta el último sector */
    uint64_t jump_sectors;       /* 0 = sin salto tras un ilegible */
    uint32_t retries;            /* 0–100 */
    uint32_t skip_code_count;    /* ≤ MR_MAX_SKIP_CODES */
    const int32_t *skip_codes;   /* códigos del sistema (> 0), p. ej. 23, 1117; NULL si count = 0 */
} mr_verify_options;

/* Opciones de mr_source_read_mapped_ex (32 bytes). struct_size = sizeof(mr_read_options); un tamaño
 * desconocido se rechaza. Patrones de relleno: 4 bytes que se repiten en todo el sector, en este orden.
 * Regla (MR_ERR_INVALID_ARGUMENT si no se cumple): ningún patrón puede tener sus cuatro bytes iguales
 * (00 00 00 00 se confundiría con sectores dispersos; FF FF FF FF y similares, con datos reales) y
 * bad_fill y skip_fill deben ser distintos. El estado de cada sector sigue siendo la única fuente de
 * verdad: el contenido de un sector MR_SECTOR_UNREADABLE o MR_SECTOR_SKIPPED nunca son datos. */
typedef struct mr_read_options {
    uint32_t struct_size;
    uint32_t flags;              /* MR_READ_READ_BACK */
    uint64_t jump_sectors;       /* 0 = sin salto tras un ilegible */
    uint32_t retries;            /* 0–100 */
    uint8_t  bad_fill[4];        /* relleno de MR_SECTOR_UNREADABLE (por defecto "BAD!") */
    uint8_t  skip_fill[4];       /* relleno de MR_SECTOR_SKIPPED (por defecto "SKIP") */
    uint32_t reserved;           /* debe ser 0 */
} mr_read_options;

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
mr_status mr_source_describe(uint64_t source, uint8_t *buf, size_t cap, size_t *out_len);
/* Lectura estricta: falla ante cualquier error de lectura. */
mr_status mr_source_read(uint64_t source, uint64_t lba, uint32_t count,
                         uint8_t *buf, size_t cap, size_t *out_len);
/* Lectura tolerante: states[i] = MR_SECTOR_* de cada sector. Devuelve MR_OK aunque haya sectores
 * ilegibles; el llamador DEBE consultar states antes de presentar o usar los datos. */
mr_status mr_source_read_mapped(uint64_t source, uint64_t lba, uint32_t count, uint32_t retries,
                                uint8_t *buf, size_t cap, uint8_t *states, size_t states_cap,
                                size_t *out_len);
/* Lectura tolerante con salto, lectura hacia atrás y patrones de relleno configurables. */
mr_status mr_source_read_mapped_ex(uint64_t source, uint64_t lba, uint32_t count,
                                   const mr_read_options *options,
                                   uint8_t *buf, size_t cap, uint8_t *states, size_t states_cap,
                                   size_t *out_len);
/* Rango de sectores de un origen como origen propio (partición o rango elegido). */
mr_status mr_source_open_range(uint64_t source, uint64_t start_lba, uint64_t count,
                               uint64_t *out_source);
mr_status mr_source_close(uint64_t source);

/* Imágenes divididas: lista los segmentos (uno por línea) sin abrirlos. */
mr_status mr_split_discover(const uint8_t *path, size_t path_len,
                            uint8_t *buf, size_t cap, size_t *out_len, uint32_t *out_count);

/* Tareas. store_path puede ser NULL (store_len = 0): sin registro en base de datos. */
mr_status mr_hash_start(const uint8_t *path, size_t path_len, uint32_t sector_size,
                        uint32_t algorithms_mask,
                        const uint8_t *store_path, size_t store_len,
                        uint64_t *out_task);
mr_status mr_source_hash_start(uint64_t source, uint32_t algorithms_mask,
                               const uint8_t *store_path, size_t store_len, uint64_t *out_task);
mr_status mr_hash_result(uint64_t task, uint32_t algorithm,
                         uint8_t *buf, size_t cap, size_t *out_len);

/* count = 0: hasta el último sector. jump_sectors = 0: sin salto tras un ilegible. */
mr_status mr_source_verify_start(uint64_t source, uint64_t start_lba, uint64_t count,
                                 uint32_t retries, uint64_t jump_sectors, uint32_t flags,
                                 uint64_t *out_task);
/* Verificación con política de omisión de errores. */
mr_status mr_source_verify_start_ex(uint64_t source, const mr_verify_options *options,
                                    uint64_t *out_task);
mr_status mr_verify_result(uint64_t task, mr_verify_report *out);
/* Sectores cuyo error se omitió por la política y nº de códigos del sistema distintos registrados. */
mr_status mr_verify_error_summary(uint64_t task, uint64_t *out_omitted, uint32_t *out_code_count);
/* Código nº index (orden creciente) y nº de sectores ilegibles cuyo último intento falló con él. */
mr_status mr_verify_error_code(uint64_t task, uint32_t index, int32_t *out_code,
                               uint64_t *out_sectors);
mr_status mr_verify_untrusted_range(uint64_t task, uint64_t index, uint64_t *out_lba,
                                    uint64_t *out_count, uint32_t *out_state);

mr_status mr_task_status_get(uint64_t task, mr_task_status *out);
mr_status mr_task_cancel(uint64_t task);
mr_status mr_task_free(uint64_t task);

#ifdef __cplusplus
}
#endif
#endif /* MEMRESCUE_CORE_H */
