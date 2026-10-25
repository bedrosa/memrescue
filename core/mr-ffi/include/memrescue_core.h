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
 * Añadido sin cambiar la ABI 3 (M2, issue #12): tablas de particiones por índice
 * (mr_source_partitions, mr_partitions_info, mr_partition_get, mr_partition_type_name,
 * mr_partition_anomaly, mr_partition_anomaly_text, mr_partition_open, mr_partitions_free)
 * con estructuras versionadas por struct_size; con GPT integrada: copia en uso, desplazamiento,
 * nombres y GUID (mr_partition_name, mr_partition_open_ex con MR_PARTITION_OPEN_APPLY_SHIFT).
 * Añadido sin cambiar la ABI 3 (M2, issue #15): búsqueda exprés de particiones perdidas como tarea
 * (mr_source_partition_search_start con mr_partition_search_options versionada; resultado por
 * índice: mr_partition_search_result, mr_found_volume_get, mr_found_volume_origin,
 * mr_found_volume_anomaly, mr_found_volume_anomaly_text, mr_found_table_get; apertura con
 * mr_found_volume_open). mr_task_status_get, mr_task_cancel y mr_task_free admiten la tarea.
 * Añadido sin cambiar la ABI 3 (M2.5, issue #17): Apple Partition Map (MR_SCHEME_APM,
 * MR_ORIGIN_APM, códigos 400-414 y mr_partition_apm_type).
 * Añadido sin cambiar la ABI 3 (M3, issue #22): sistema de archivos NTFS (mr_ntfs_*): volumen,
 * listado de directorios por índice, rutas, árbol como tarea, un fichero como origen y copia de
 * un fichero como tarea; códigos 15 (MR_ERR_NOT_FOUND) y 16 (MR_ERR_ALREADY_EXISTS), que solo
 * devuelven estas funciones.
 * Añadido sin cambiar la ABI 3 (M1.5, issue #11): VMDK con descriptor separado y extensiones
 * aprobadas explícitamente (mr_vmdk_extents_list, mr_vmdk_extents_info, mr_vmdk_extents_describe,
 * mr_vmdk_extents_issue, mr_vmdk_extent_get, mr_vmdk_extent_name, mr_vmdk_extent_issue,
 * mr_vmdk_extents_free y mr_source_open_vmdk con mr_approved_file versionada).
 * Añadido sin cambiar la ABI 3 (M1.5, issue #8): ficheros externos aprobados por el usuario
 * (mr_source_open_ex con mr_open_options versionada y lista de mr_str; hoy, la cadena de padres
 * de un disco diferencial VHD/VHDX), MR_OPEN_IGNORE_LINK_MISMATCH, mr_image_external_hints y el
 * código MR_ERR_EXTERNAL_MISMATCH (17).
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
#define MR_ERR_NOT_FOUND          15   /* ruta NTFS que no existe (#22) */
#define MR_ERR_ALREADY_EXISTS     16   /* la salida ya existe y no se pidió sustituirla (#22) */
#define MR_ERR_EXTERNAL_MISMATCH  17   /* un fichero externo indicado no corresponde (padre de otro
                                          disco, tamaño o sector distintos, ciclo, cadena > 16) */
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
#define MR_OPEN_IGNORE_LINK_MISMATCH 8u /* solo mr_source_open_ex: aceptar un fichero externo cuyo
                                     vínculo de identidad no coincide (petición explícita del
                                     usuario); mr_source_describe lo advierte con «NO verificado» */
#define MR_MAX_EXTERNAL_FILES 64u  /* máximo de elementos en mr_open_options.externals */

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

/* Cadena UTF-8 de entrada (16 bytes en x64), sin terminador. */
typedef struct mr_str {
    const uint8_t *ptr;
    size_t len;
} mr_str;

/* Opciones de mr_source_open_ex (32 bytes en x64). struct_size = sizeof(mr_open_options); un
 * tamaño desconocido se rechaza. externals: ficheros externos que el usuario eligió y confirmó, en
 * orden (disco diferencial: padre inmediato primero, hasta el base). El motor nunca abre otros:
 * cada ruta pasa la política de rutas y se abre en solo lectura; si no corresponde
 * (MR_ERR_EXTERNAL_MISMATCH), si falta un padre (MR_ERR_EXTERNAL_REFERENCE) o si la imagen no usa
 * ficheros externos y la lista no está vacía (MR_ERR_EXTERNAL_MISMATCH), la apertura falla. */
typedef struct mr_open_options {
    uint32_t struct_size;
    uint32_t flags;              /* MR_OPEN_* (MR_OPEN_SPLIT no admite externals) */
    uint32_t sector_size;        /* 0 = nativo */
    uint32_t external_count;     /* ≤ MR_MAX_EXTERNAL_FILES */
    const mr_str *externals;     /* NULL si external_count = 0 */
    uint64_t reserved;           /* debe ser 0 */
} mr_open_options;

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
/* Apertura con opciones versionadas y ficheros externos aprobados por el usuario. */
mr_status mr_source_open_ex(const uint8_t *path, size_t path_len, const mr_open_options *options,
                            uint64_t *out_source);
/* Indicaciones del contenedor sobre los ficheros externos que necesita (padre de un disco
 * diferencial), sin abrir ningún otro fichero: una línea "clave\tvalor" por indicación; vacía si
 * no necesita ninguno. Solo para mostrarlas: nunca se usan como rutas. */
mr_status mr_image_external_hints(const uint8_t *path, size_t path_len,
                                  uint8_t *buf, size_t cap, size_t *out_len, uint32_t *out_count);
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

/* ---------- Particiones (añadido sin cambiar la ABI 3) ----------
 * mr_source_partitions lee la tabla del origen (MBR/EBR; MBR protector o híbrido: solo lo que ve
 * el MBR) y devuelve un handle que se consulta por índice y se libera con mr_partitions_free.
 * Lectura tolerante: un sector ilegible es una anomalía (MR_ANOMALY_UNREADABLE_SECTOR), nunca se
 * interpreta su relleno. Las estructuras de salida son versionadas: el llamador escribe
 * struct_size = sizeof(...) antes de la llamada; otro tamaño se rechaza (MR_ERR_INVALID_ARGUMENT). */
#define MR_SCHEME_NONE    0u
#define MR_SCHEME_MBR     1u
#define MR_SCHEME_GPT     2u   /* GPT (MBR protector, o firma GPT sin MBR utilizable) */
#define MR_SCHEME_HYBRID  3u   /* MBR híbrido: entradas MBR distintas de 0xEE y después las GPT */
#define MR_SCHEME_APM     4u   /* Apple Partition Map: LBA en bloques del mapa (sector_size) */

#define MR_CONFIDENCE_VALID             1u
#define MR_CONFIDENCE_VALID_WITH_FIXES  2u
#define MR_CONFIDENCE_SUSPICIOUS        3u
#define MR_CONFIDENCE_INVALID           4u

#define MR_SEVERITY_NOTICE      1u
#define MR_SEVERITY_SUSPICIOUS  2u
#define MR_SEVERITY_INVALID     3u
#define MR_SEVERITY_COMPENSATED 4u   /* daño compensado por la otra copia GPT (válida con correcciones) */

/* mr_partition_table_info.gpt_selected y gpt_flags */
#define MR_GPT_NONE     0u   /* sin GPT o ninguna copia utilizable */
#define MR_GPT_PRIMARY  1u
#define MR_GPT_BACKUP   2u   /* copia de respaldo en uso */
#define MR_GPT_PRESENT  1u   /* gpt_flags: hay lectura GPT */

/* mr_partition_open_ex: abrir una partición GPT de una imagen desplazada en LBA disco - shift. */
#define MR_PARTITION_OPEN_APPLY_SHIFT 1u

#define MR_ORIGIN_PRIMARY  1u
#define MR_ORIGIN_LOGICAL  2u
#define MR_ORIGIN_GPT      3u
#define MR_ORIGIN_APM      4u   /* slot = número de entrada del mapa (desde 1) */

#define MR_PARTITION_TABLE     0xFFFFFFFFu  /* partition_index de las anomalías de la tabla */
#define MR_ANOMALY_HAS_TARGET  1u           /* mr_partition_anomaly.flags: target_lba válido */
#define MR_ANOMALY_HAS_VALUE   2u           /* mr_partition_anomaly.flags: value válido */
#define MR_ANOMALY_VALUE_SIGNED 4u          /* value es int64_t en complemento a dos */

/* Códigos de anomalía (mr_partition_anomaly.code); el texto estable va en mr_partition_anomaly_text. */
#define MR_ANOMALY_UNREADABLE_SECTOR           1u    /* unreadable_sector */
#define MR_ANOMALY_MBR_MISSING               100u    /* mbr_missing: sin tabla reconocible */
#define MR_ANOMALY_MBR_EMPTY                 101u    /* mbr_empty: firma sin entradas */
#define MR_ANOMALY_MBR_SIGNATURE_MISSING     102u
#define MR_ANOMALY_MBR_INVALID_BOOT_INDICATOR 103u   /* value = indicador */
#define MR_ANOMALY_MBR_MULTIPLE_ACTIVE       104u
#define MR_ANOMALY_MBR_MULTIPLE_EXTENDED     105u
#define MR_ANOMALY_MBR_OUTSIDE_DISK          106u    /* value = LBA inicial */
#define MR_ANOMALY_MBR_PARTIALLY_OUTSIDE     107u    /* value = final (exclusivo) */
#define MR_ANOMALY_MBR_OVERLAP               108u
#define MR_ANOMALY_MBR_UNKNOWN_TYPE          109u    /* value = tipo */
#define MR_ANOMALY_MBR_ZERO_LENGTH           110u    /* value = tipo */
#define MR_ANOMALY_EBR_LOOP                  111u    /* target_lba = EBR repetido */
#define MR_ANOMALY_EBR_LINK_OUTSIDE_DISK     112u    /* target_lba = destino */
#define MR_ANOMALY_EBR_LINK_OUTSIDE_EXTENDED 113u    /* target_lba = destino */
#define MR_ANOMALY_EBR_CHAIN_LIMIT           114u    /* value = límite */
#define MR_ANOMALY_EBR_LOGICAL_OUTSIDE_EXT   115u
#define MR_ANOMALY_EBR_INVALID               116u
#define MR_ANOMALY_EBR_EXTRA_ENTRIES         117u
#define MR_ANOMALY_PROTECTIVE_MBR_START      118u    /* value = LBA inicial declarada */
#define MR_ANOMALY_PROTECTIVE_SIZE_MISMATCH  119u    /* value = tamaño declarado */
#define MR_ANOMALY_HYBRID_MBR                120u
#define MR_ANOMALY_SUPERFLOPPY               121u
#define MR_ANOMALY_HYBRID_MBR_MISMATCH       122u    /* entrada del híbrido sin partición GPT igual */
/* GPT (bloque 2xx): las de una copia usan el código impar para la primaria y el siguiente para la
 * de respaldo. lba = posición en la imagen; *_OUTSIDE_IMAGE: lba no válido, value = LBA del disco. */
#define MR_ANOMALY_GPT_HEADER_MISSING        201u    /* 202 respaldo */
#define MR_ANOMALY_GPT_HEADER_OUTSIDE_IMAGE  203u    /* 204 respaldo; value = LBA del disco */
#define MR_ANOMALY_GPT_HEADER_CRC            205u    /* 206 respaldo */
#define MR_ANOMALY_GPT_HEADER_SIZE           207u    /* 208 respaldo; value = tamaño */
#define MR_ANOMALY_GPT_REVISION_UNSUPPORTED  209u    /* 210 respaldo; value = revisión */
#define MR_ANOMALY_GPT_REVISION_MINOR        211u    /* 212 respaldo; value = revisión */
#define MR_ANOMALY_GPT_MY_LBA_MISMATCH       213u    /* 214 respaldo; value = MyLBA, target = esperada */
#define MR_ANOMALY_GPT_ALTERNATE_LBA         215u    /* 216 respaldo; value = AlternateLBA */
#define MR_ANOMALY_GPT_USABLE_RANGE_INVALID  217u    /* 218 respaldo */
#define MR_ANOMALY_GPT_ENTRIES_LOCATION      219u    /* 220 respaldo; value = LBA de las entradas */
#define MR_ANOMALY_GPT_ENTRY_ARRAY_LIMITS    221u    /* 222 respaldo; value = nº de entradas */
#define MR_ANOMALY_GPT_ENTRIES_CRC           223u    /* 224 respaldo */
#define MR_ANOMALY_GPT_ENTRIES_OUTSIDE_IMAGE 225u    /* 226 respaldo; value = LBA del disco */
#define MR_ANOMALY_GPT_BACKUP_IN_USE         230u
#define MR_ANOMALY_GPT_COPIES_DISAGREE       231u
#define MR_ANOMALY_IMAGE_SHIFTED             232u    /* value = desplazamiento (con signo) */
#define MR_ANOMALY_GPT_IMAGE_TRUNCATED       233u    /* value = sectores que faltan */
#define MR_ANOMALY_GPT_USABLE_OUTSIDE_IMAGE  234u
#define MR_ANOMALY_GPT_SECTOR_SIZE_MISMATCH  235u    /* value = tamaño de sector usado */
#define MR_ANOMALY_GPT_ENTRY_INVERTED        236u    /* value = índice de entrada */
#define MR_ANOMALY_GPT_ENTRY_OUTSIDE_USABLE  237u    /* value = índice de entrada */
#define MR_ANOMALY_GPT_ENTRY_OUTSIDE_IMAGE   238u    /* value = índice de entrada */
#define MR_ANOMALY_GPT_ENTRY_OVERLAP         239u    /* value = índice de entrada */
#define MR_ANOMALY_GPT_DUPLICATE_UNIQUE_GUID 240u    /* value = índice de entrada */
#define MR_ANOMALY_GPT_NAME_INVALID          241u    /* value = índice de entrada */

/* Apple Partition Map (#17): lba = sector del origen del bloque afectado; value = nº de entrada
 * salvo donde se indica. */
#define MR_ANOMALY_APM_DDM_MISSING           400u    /* bloque 0 sin firma ER */
#define MR_ANOMALY_APM_BLOCK_SIZE_INVALID    401u    /* value = sbBlkSize declarado */
#define MR_ANOMALY_APM_BLOCK_SIZE_MISMATCH   402u    /* value = sbBlkSize declarado */
#define MR_ANOMALY_APM_DISK_SIZE_MISMATCH    403u    /* value = sbBlkCount declarado */
#define MR_ANOMALY_APM_MAP_MISSING           404u    /* DDM sin entrada PM en el bloque 1 */
#define MR_ANOMALY_APM_MAP_COUNT_INVALID     405u    /* value = pmMapBlkCnt (0 o > 512) */
#define MR_ANOMALY_APM_MAP_COUNT_MISMATCH    406u    /* value = pmMapBlkCnt de la entrada */
#define MR_ANOMALY_APM_MAP_OUTSIDE_IMAGE     407u    /* sin lba; value = primera entrada que falta */
#define MR_ANOMALY_APM_ENTRY_SIGNATURE       408u    /* entrada sin firma PM (no se lista) */
#define MR_ANOMALY_APM_ENTRY_ZERO_LENGTH     409u
#define MR_ANOMALY_APM_ENTRY_OUTSIDE_DISK    410u
#define MR_ANOMALY_APM_ENTRY_PARTIALLY_OUTSIDE 411u
#define MR_ANOMALY_APM_ENTRY_OVERLAP         412u
#define MR_ANOMALY_APM_ENTRY_DATA_OUTSIDE    413u
#define MR_ANOMALY_APM_SECTOR_SIZE_MISMATCH  414u    /* sin lba; value = tamaño de bloque del mapa */

typedef struct mr_partition_table_info {  /* 80 bytes */
    uint32_t struct_size;        /* = sizeof(mr_partition_table_info), lo escribe el llamador */
    uint32_t scheme;             /* MR_SCHEME_* */
    uint32_t sector_size;        /* unidades de las LBA de las particiones */
    uint32_t confidence;         /* MR_CONFIDENCE_* del conjunto */
    uint64_t partition_count;
    uint64_t anomaly_count;      /* anomalías de la tabla (partition_index = MR_PARTITION_TABLE) */
    uint32_t gpt_selected;       /* MR_GPT_NONE / MR_GPT_PRIMARY / MR_GPT_BACKUP */
    uint32_t gpt_flags;          /* MR_GPT_PRESENT */
    int64_t  shift;              /* LBA disco = LBA imagen + shift (las particiones GPT llevan LBA del disco) */
    uint64_t trailing_sectors_missing;
    uint32_t gpt_sector_size;    /* 0 sin GPT */
    uint32_t reserved;
    uint8_t  disk_guid[16];      /* orden del disco; ceros si no hay */
} mr_partition_table_info;

typedef struct mr_partition_info {  /* 96 bytes */
    uint32_t struct_size;        /* = sizeof(mr_partition_info), lo escribe el llamador */
    uint32_t origin;             /* MR_ORIGIN_* */
    uint32_t slot;               /* entrada de su tabla (0-3), índice GPT o entrada APM (desde 1) */
    uint32_t mbr_type;           /* 0 en GPT y APM */
    uint64_t ebr_lba;            /* EBR de una lógica; 0 en las demás */
    uint64_t start_lba;
    uint64_t sector_count;
    uint32_t active;             /* 1 = indicador 0x80 */
    uint32_t confidence;         /* MR_CONFIDENCE_* */
    uint32_t anomaly_count;      /* anomalías propias (mr_partition_anomaly con su índice) */
    uint32_t reserved;
    uint8_t  gpt_type[16];       /* GUID de tipo en el orden del disco; ceros en MBR */
    uint8_t  unique_guid[16];    /* GUID único GPT en el orden del disco; ceros en MBR */
    uint64_t attributes;         /* atributos GPT; pmPartStatus en APM; 0 en MBR */
} mr_partition_info;

typedef struct mr_partition_anomaly {  /* 40 bytes */
    uint32_t struct_size;        /* = sizeof(mr_partition_anomaly), lo escribe el llamador */
    uint32_t code;               /* MR_ANOMALY_* */
    uint32_t severity;           /* MR_SEVERITY_* */
    uint32_t flags;              /* MR_ANOMALY_HAS_TARGET | MR_ANOMALY_HAS_VALUE */
    uint64_t lba;                /* sector de tabla donde se detectó */
    uint64_t target_lba;
    uint64_t value;
} mr_partition_anomaly;

mr_status mr_source_partitions(uint64_t source, uint64_t *out_table);
mr_status mr_partitions_info(uint64_t table, mr_partition_table_info *out);
mr_status mr_partition_get(uint64_t table, uint32_t index, mr_partition_info *out);
/* Nombre legible del tipo (cadena vacía si es desconocido). */
mr_status mr_partition_type_name(uint64_t table, uint32_t index,
                                 uint8_t *buf, size_t cap, size_t *out_len);
/* Nombre GPT o APM de la partición (cadena vacía en MBR). */
mr_status mr_partition_name(uint64_t table, uint32_t index,
                            uint8_t *buf, size_t cap, size_t *out_len);
/* Tipo APM tal como está en el mapa (p. ej. "Apple_HFS"); cadena vacía si no es APM. */
mr_status mr_partition_apm_type(uint64_t table, uint32_t index,
                                uint8_t *buf, size_t cap, size_t *out_len);
/* partition_index = MR_PARTITION_TABLE para las anomalías de la tabla. */
mr_status mr_partition_anomaly(uint64_t table, uint32_t partition_index, uint32_t anomaly_index,
                               mr_partition_anomaly *out);
/* "<código estable>\n<descripción en español>". */
mr_status mr_partition_anomaly_text(uint64_t table, uint32_t partition_index,
                                    uint32_t anomaly_index,
                                    uint8_t *buf, size_t cap, size_t *out_len);
/* Partición como origen (vista acotada; si termina fuera del origen se abre la parte existente y
 * mr_source_describe lo indica). Se cierra con mr_source_close. */
mr_status mr_partition_open(uint64_t table, uint32_t index, uint64_t *out_source);
/* Una partición GPT de una imagen desplazada (shift != 0) se rechaza con mr_partition_open; con
 * MR_PARTITION_OPEN_APPLY_SHIFT se abre en LBA disco - shift y mr_source_describe lo indica. Una
 * tabla interpretada con otro tamaño de sector que el del origen se rechaza siempre. */
mr_status mr_partition_open_ex(uint64_t table, uint32_t index, uint32_t flags,
                               uint64_t *out_source);
mr_status mr_partitions_free(uint64_t table);

/*
 * Búsqueda exprés de particiones perdidas (M2, issue #15, ADR-023). Solo a petición. La tarea lee
 * la tabla del origen y sondea un número acotado de posiciones candidatas (nunca recorre el disco):
 * progreso de mr_task_status_get = lecturas hechas / max_reads. Al cancelar, la tarea termina en
 * estado cancelado (4) CON resultado parcial (status = MR_SEARCH_CANCELLED). Los sectores
 * ilegibles nunca son evidencia: el indicador queda MR_CHECK_UNVERIFIABLE.
 */
#define MR_SEARCH_FAMILY_NTFS    1u
#define MR_SEARCH_FAMILY_FAT     2u   /* FAT12, FAT16 y FAT32 */
#define MR_SEARCH_FAMILY_EXFAT   4u
#define MR_SEARCH_FAMILY_MBR     8u   /* tablas MBR/EBR en posiciones candidatas (modo estricto) */
#define MR_SEARCH_FAMILY_GPT    16u   /* cabeceras GPT en posiciones candidatas */

typedef struct mr_partition_search_options {  /* 64 bytes; un campo a 0 = valor por defecto */
    uint32_t struct_size;        /* = sizeof(mr_partition_search_options) */
    uint32_t families;           /* MR_SEARCH_FAMILY_*; 0 = todas */
    uint64_t max_reads;          /* lecturas físicas máximas (4096) */
    uint64_t max_bytes;          /* bytes leídos máximos (32 MiB) */
    uint64_t start_grid_bytes;   /* ventana inicial con múltiplos de 1 MiB (64 MiB) */
    uint64_t end_grid_bytes;     /* ventana final con finales de MiB (16 MiB) */
    uint32_t max_volumes;        /* volúmenes tras los que se deja de buscar (256) */
    uint32_t probe_window;       /* bytes leídos por candidato, múltiplo de 512 (8192) */
    uint32_t cylinders;          /* límites de cilindro CHS (64) */
    uint32_t probes_per_gap;     /* candidatos por hueco sin explicar (8) */
    uint32_t flags;              /* reservado: 0 */
    uint32_t reserved;           /* 0 */
} mr_partition_search_options;

#define MR_SEARCH_COMPLETE          1u
#define MR_SEARCH_BUDGET_EXHAUSTED  2u   /* presupuesto, cupo de un nivel o max_volumes: parcial */
#define MR_SEARCH_CANCELLED         3u
#define MR_SEARCH_HAS_SHIFT         1u   /* mr_partition_search_info.flags: shift_bytes válido */
#define MR_SEARCH_VOLUME_LIMIT      2u

typedef struct mr_partition_search_info {  /* 96 bytes */
    uint32_t struct_size;        /* = sizeof(mr_partition_search_info) */
    uint32_t status;             /* MR_SEARCH_COMPLETE / _BUDGET_EXHAUSTED / _CANCELLED */
    uint32_t volume_count;
    uint32_t table_count;        /* tablas encontradas fuera de la tabla principal */
    uint32_t anomaly_count;      /* de la búsqueda: volume_index = MR_PARTITION_TABLE */
    uint32_t flags;              /* MR_SEARCH_HAS_SHIFT | MR_SEARCH_VOLUME_LIMIT */
    int64_t  shift_bytes;        /* posición en la imagen - posición en el disco */
    uint64_t reads;
    uint64_t max_reads;
    uint64_t bytes_read;
    uint64_t unverifiable_sectors;
    uint64_t probed;             /* candidatos sondeados */
    uint64_t pending;            /* candidatos sin sondear */
    uint32_t sector_size;
    uint32_t reserved;
    uint64_t sectors;
} mr_partition_search_info;

#define MR_FS_NTFS   1u
#define MR_FS_FAT12  2u
#define MR_FS_FAT16  3u
#define MR_FS_FAT32  4u
#define MR_FS_EXFAT  5u
/* Indicadores (mr_found_volume.primary ... aligned). */
#define MR_CHECK_YES            1u   /* + */
#define MR_CHECK_NO             2u   /* x: ausente o dañada */
#define MR_CHECK_UNVERIFIABLE   3u   /* ?: sector ilegible, no se interpretó */
#define MR_CHECK_OUTSIDE        4u   /* o: fuera de la imagen */
#define MR_CHECK_NOT_APPLICABLE 5u   /* -: no aplica */
#define MR_FOUND_PARTIAL        1u   /* empieza antes de la imagen: no se puede abrir */
#define MR_FOUND_LISTED         2u   /* una partición de la tabla lo describe (table_index) */
#define MR_FOUND_REQUIRES_SHIFT 4u   /* abrirlo exige MR_PARTITION_OPEN_APPLY_SHIFT */
#define MR_FOUND_HAS_BACKUP     8u   /* backup_byte válido */
#define MR_ANOMALY_LBA_SIGNED   8u   /* mr_partition_anomaly.flags: lba es int64_t */
/* Anomalías de la búsqueda (mr_found_volume_anomaly.code). */
#define MR_ANOMALY_FOUND_PRIMARY_DAMAGED      301u  /* restaurable desde la copia */
#define MR_ANOMALY_FOUND_PRIMARY_UNREADABLE   302u
#define MR_ANOMALY_FOUND_PRIMARY_OUTSIDE      303u  /* volumen parcial */
#define MR_ANOMALY_FOUND_BACKUP_MISSING       304u
#define MR_ANOMALY_FOUND_BACKUP_UNREADABLE    305u
#define MR_ANOMALY_FOUND_BACKUP_OUTSIDE       306u
#define MR_ANOMALY_FOUND_BACKUP_MISMATCH      307u
#define MR_ANOMALY_FOUND_BEYOND_IMAGE_END     308u  /* value = bytes que faltan */
#define MR_ANOMALY_FOUND_SECTOR_SIZE_MISMATCH 309u  /* value = sector del FS */
#define MR_ANOMALY_FOUND_UNALIGNED            310u
#define MR_ANOMALY_FOUND_SHIFT_DETECTED       311u  /* value = desplazamiento en bytes */
#define MR_ANOMALY_FOUND_SELF_LOCATION        312u  /* value = byte declarado */
#define MR_ANOMALY_FOUND_OVERLAP              313u
#define MR_ANOMALY_FOUND_UNCONFIRMED          314u
#define MR_ANOMALY_SEARCH_IMAGE_SHIFTED       320u  /* lba = LBA 0 del disco; value = sectores que faltan */
#define MR_ANOMALY_SEARCH_MBR_DISPLACED       321u  /* lba = tabla encontrada */

typedef struct mr_found_volume {  /* 136 bytes */
    uint32_t struct_size;        /* = sizeof(mr_found_volume) */
    uint32_t fs;                 /* MR_FS_* */
    int64_t  start_byte;         /* negativo si empieza antes de la imagen */
    uint64_t length_bytes;
    int64_t  start_lba;          /* sectores del origen, redondeo hacia abajo */
    uint64_t sector_count;
    uint32_t fs_sector_size;
    uint32_t confidence;         /* MR_CONFIDENCE_* */
    uint32_t primary;            /* MR_CHECK_*: principal (P) */
    uint32_t backup;             /* copia coherente (C) */
    uint32_t structures;         /* estructuras básicas (S) */
    uint32_t entry;              /* entrada de tabla (E) */
    uint32_t self_location;      /* inicio declarado coherente (U) */
    uint32_t aligned;            /* alineado a 1 MiB o CHS (A) */
    uint32_t tier;               /* 0-4 = P0-P4 */
    uint32_t flags;              /* MR_FOUND_* */
    uint32_t table_index;        /* partición de la tabla, o MR_PARTITION_TABLE */
    uint32_t found_table_index;  /* tabla encontrada, o MR_PARTITION_TABLE */
    uint32_t anomaly_count;
    uint32_t suggested_mbr_type;
    int64_t  primary_byte;
    int64_t  backup_byte;        /* con MR_FOUND_HAS_BACKUP */
    uint64_t declared_start;     /* inicio declarado por el FS en sus sectores (0 = no) */
    uint64_t serial;
    uint32_t score;
    uint32_t reserved;
} mr_found_volume;

#define MR_FOUND_TABLE_MBR 1u
#define MR_FOUND_TABLE_GPT 2u
typedef struct mr_found_table {  /* 32 bytes */
    uint32_t struct_size;
    uint32_t kind;               /* MR_FOUND_TABLE_* */
    uint64_t lba;
    uint64_t declared_lba;       /* MyLBA de la cabecera GPT; 0 en MBR */
    uint32_t entry_count;
    uint32_t reserved;
} mr_found_table;

/* options puede ser NULL (valores por defecto). */
mr_status mr_source_partition_search_start(uint64_t source,
                                           const mr_partition_search_options *options,
                                           uint64_t *out_task);
mr_status mr_partition_search_result(uint64_t task, mr_partition_search_info *out);
mr_status mr_found_volume_get(uint64_t task, uint32_t index, mr_found_volume *out);
/* Qué candidato propuso el volumen (texto). */
mr_status mr_found_volume_origin(uint64_t task, uint32_t index,
                                 uint8_t *buf, size_t cap, size_t *out_len);
/* volume_index = MR_PARTITION_TABLE para las anomalías de la búsqueda. */
mr_status mr_found_volume_anomaly(uint64_t task, uint32_t volume_index, uint32_t anomaly_index,
                                  mr_partition_anomaly *out);
mr_status mr_found_volume_anomaly_text(uint64_t task, uint32_t volume_index,
                                       uint32_t anomaly_index,
                                       uint8_t *buf, size_t cap, size_t *out_len);
mr_status mr_found_table_get(uint64_t task, uint32_t index, mr_found_table *out);
/* Volumen como origen. Parcial o desalineado: MR_ERR_INVALID_ARGUMENT siempre; con
 * MR_FOUND_REQUIRES_SHIFT solo con MR_PARTITION_OPEN_APPLY_SHIFT. Se cierra con mr_source_close. */
mr_status mr_found_volume_open(uint64_t task, uint32_t index, uint32_t flags,
                               uint64_t *out_source);

/*
 * Sistema de archivos NTFS (M3, issue #22). Solo lectura. Handles del mismo registro que
 * orígenes y tareas; estructuras de salida versionadas por struct_size; consultas por índice.
 * Un volumen sobrevive al cierre de su origen; los listados y tareas, a la liberación del volumen.
 * Cada entrada de directorio está contrastada con la MFT: solo MR_NTFS_ENTRY_LIVE es un fichero
 * vivo; las demás son entradas del índice cuyo registro está libre, reutilizado, ilegible o
 * dañado (16-31 reservados para borrados y residuales, #23).
 */
#define MR_NTFS_BOOT_FROM_BACKUP    1u
#define MR_NTFS_VERSION_KNOWN       2u
#define MR_NTFS_UPCASE_FROM_VOLUME  4u   /* si no, plegado simple de mayúsculas */

typedef struct mr_ntfs_volume_info {  /* 72 bytes */
    uint32_t struct_size;        /* = sizeof(mr_ntfs_volume_info) */
    uint32_t confidence;         /* MR_CONFIDENCE_* */
    uint64_t serial;
    uint64_t record_count;       /* registros de la MFT */
    uint64_t total_clusters;
    uint32_t cluster_size;
    uint32_t record_size;
    uint32_t index_record_size;
    uint32_t bytes_per_sector;
    uint32_t version_major;      /* con MR_NTFS_VERSION_KNOWN */
    uint32_t version_minor;
    uint32_t flags;              /* MR_NTFS_BOOT_FROM_BACKUP | VERSION_KNOWN | UPCASE_FROM_VOLUME */
    uint32_t anomaly_count;      /* mr_ntfs_volume_anomaly_text */
    uint64_t mft_lcn;
} mr_ntfs_volume_info;

#define MR_NTFS_ENTRY_LIVE              1u
#define MR_NTFS_ENTRY_STALE_NOT_IN_USE  2u   /* registro libre: entrada residual */
#define MR_NTFS_ENTRY_STALE_REUSED      3u   /* registro reutilizado (otra secuencia) */
#define MR_NTFS_ENTRY_RECORD_UNUSABLE   4u   /* registro dañado, ilegible...: record_state */
#define MR_NTFS_ENTRY_INVALID_REFERENCE 5u   /* fuera de la MFT o registro de extensión */
#define MR_NTFS_ENTRY_DIRECTORY         1u   /* mr_ntfs_entry.flags */
#define MR_NTFS_ENTRY_SYSTEM            2u   /* registros 0-15 y contenido de $Extend */
#define MR_NTFS_ENTRY_NAME_LOSSY        4u   /* unidades UTF-16 sueltas sustituidas por U+FFFD */
#define MR_NTFS_ENTRY_HAS_DOS_NAME      8u
#define MR_NTFS_ENTRY_TIMES_FROM_INDEX 16u   /* fechas de la clave del índice, no del registro */

typedef struct mr_ntfs_entry {  /* 104 bytes */
    uint32_t struct_size;        /* = sizeof(mr_ntfs_entry) */
    uint32_t state;              /* MR_NTFS_ENTRY_LIVE ... */
    uint64_t record;
    uint64_t parent_record;
    uint32_t sequence;           /* de la referencia del índice */
    uint32_t parent_sequence;
    uint64_t size;               /* $DATA principal; 0 en directorios */
    uint64_t allocated_size;
    uint64_t created;            /* FILETIME UTC */
    uint64_t modified;
    uint64_t mft_modified;
    uint64_t accessed;
    uint32_t file_attributes;    /* atributos DOS */
    uint32_t flags;              /* MR_NTFS_ENTRY_DIRECTORY ... */
    uint32_t name_space;         /* 0 POSIX, 1 Win32, 2 DOS, 3 Win32 y DOS */
    uint32_t link_count;
    uint32_t record_state;       /* RECORD_UNUSABLE: 1 en uso, 2 no en uso, 3 vacío, 4 sin
                                    inicializar, 5 sin mapa, 6 fuera del origen, 7 ilegible,
                                    8 dañado */
    uint32_t found_sequence;     /* secuencia actual del registro (0 si no se leyó) */
} mr_ntfs_entry;

typedef struct mr_ntfs_listing_info {  /* 32 bytes */
    uint32_t struct_size;
    uint32_t entry_count;
    uint64_t directory_record;
    uint32_t directory_sequence;
    uint32_t anomaly_count;      /* mr_ntfs_listing_anomaly_text */
    uint32_t live_count;
    uint32_t unreadable;         /* 1 si algún nodo o registro tenía sectores no fiables */
} mr_ntfs_listing_info;

#define MR_NTFS_TREE_NO_ORPHANS   1u   /* mr_ntfs_tree_start: sin búsqueda de huérfanos */
#define MR_NTFS_TREE_COMPLETE     1u
#define MR_NTFS_TREE_LIMIT        2u
#define MR_NTFS_TREE_CANCELLED    3u
#define MR_NTFS_ROOT_PARENT       UINT64_MAX

typedef struct mr_ntfs_tree_info {  /* 40 bytes */
    uint32_t struct_size;
    uint32_t status;             /* MR_NTFS_TREE_COMPLETE / _LIMIT / _CANCELLED */
    uint64_t node_count;         /* el nodo 0 es la raíz */
    uint64_t orphan_count;
    uint32_t anomaly_count;
    uint32_t reserved;
    uint64_t directories;
} mr_ntfs_tree_info;

/* Motivo de un huérfano (registro en uso no alcanzado desde la raíz). */
#define MR_NTFS_ORPHAN_NO_NAME              1u
#define MR_NTFS_ORPHAN_PARENT_OUT_OF_RANGE  2u
#define MR_NTFS_ORPHAN_PARENT_UNUSABLE      3u
#define MR_NTFS_ORPHAN_PARENT_NOT_IN_USE    4u
#define MR_NTFS_ORPHAN_PARENT_REUSED        5u
#define MR_NTFS_ORPHAN_PARENT_NOT_DIRECTORY 6u
#define MR_NTFS_ORPHAN_NOT_INDEXED          7u
#define MR_NTFS_ORPHAN_PARENT_ORPHAN        8u
#define MR_NTFS_ORPHAN_LOOP                 9u
#define MR_NTFS_ORPHAN_TOO_DEEP            10u

typedef struct mr_ntfs_orphan {  /* 40 bytes */
    uint32_t struct_size;
    uint32_t reason;             /* MR_NTFS_ORPHAN_* */
    uint64_t record;
    uint64_t parent_record;
    uint32_t sequence;
    uint32_t parent_sequence;
    uint32_t flags;              /* MR_NTFS_ENTRY_DIRECTORY */
    uint32_t reserved;
} mr_ntfs_orphan;

#define MR_NTFS_COPY_OVERWRITE          1u   /* mr_ntfs_copy_start: sustituir la salida */
#define MR_NTFS_COPY_RANGES_TRUNCATED   1u   /* mr_ntfs_copy_report.flags */
#define MR_NTFS_COPY_RECORD_NOT_IN_USE  2u

typedef struct mr_ntfs_copy_report {  /* 56 bytes */
    uint32_t struct_size;
    uint32_t untrusted_range_count;
    uint64_t bytes;
    uint64_t untrusted_bytes;    /* no fiables: patrón de relleno BAD! o posiblemente sobrescritos */
    uint64_t sparse_bytes;
    uint64_t uninitialized_bytes;
    uint64_t possibly_overwritten_bytes; /* parte de untrusted_bytes: clústeres quizá reutilizados */
    uint32_t flags;
    uint32_t reserved;
} mr_ntfs_copy_report;

mr_status mr_ntfs_open(uint64_t source, uint64_t *out_volume);
mr_status mr_ntfs_volume_info(uint64_t volume, mr_ntfs_volume_info *out);
mr_status mr_ntfs_volume_label(uint64_t volume, uint8_t *buf, size_t cap, size_t *out_len);
/* "<código>\n<descripción>" */
mr_status mr_ntfs_volume_anomaly_text(uint64_t volume, uint32_t index,
                                      uint8_t *buf, size_t cap, size_t *out_len);
mr_status mr_ntfs_volume_free(uint64_t volume);
/* record 5 = raíz. Registro que no es un directorio: MR_ERR_INVALID_ARGUMENT. */
mr_status mr_ntfs_list_dir(uint64_t volume, uint64_t record, uint64_t *out_listing);
mr_status mr_ntfs_listing_info(uint64_t listing, mr_ntfs_listing_info *out);
mr_status mr_ntfs_listing_entry(uint64_t listing, uint32_t index, mr_ntfs_entry *out);
mr_status mr_ntfs_listing_entry_name(uint64_t listing, uint32_t index,
                                     uint8_t *buf, size_t cap, size_t *out_len);
mr_status mr_ntfs_listing_anomaly_text(uint64_t listing, uint32_t index,
                                       uint8_t *buf, size_t cap, size_t *out_len);
mr_status mr_ntfs_listing_free(uint64_t listing);
/* Ruta absoluta sin distinguir mayúsculas; no existe: MR_ERR_NOT_FOUND. */
mr_status mr_ntfs_resolve(uint64_t volume, const uint8_t *path, size_t path_len,
                          mr_ntfs_entry *out);
/* Huérfano: MR_ERR_CORRUPT con el motivo en mr_last_error. */
mr_status mr_ntfs_path_of(uint64_t volume, uint64_t record,
                          uint8_t *buf, size_t cap, size_t *out_len);
/* Tarea: progreso = entradas listadas + registros revisados; cancelada con árbol parcial. */
mr_status mr_ntfs_tree_start(uint64_t volume, uint32_t flags, uint64_t *out_task);
mr_status mr_ntfs_tree_result(uint64_t task, mr_ntfs_tree_info *out);
mr_status mr_ntfs_tree_node(uint64_t task, uint64_t index, uint64_t *out_parent,
                            uint32_t *out_depth, mr_ntfs_entry *out_entry);
mr_status mr_ntfs_tree_node_path(uint64_t task, uint64_t index,
                                 uint8_t *buf, size_t cap, size_t *out_len);
/* "<código>\n<ruta del directorio>\n<descripción>" */
mr_status mr_ntfs_tree_anomaly_text(uint64_t task, uint32_t index,
                                    uint8_t *buf, size_t cap, size_t *out_len);
mr_status mr_ntfs_tree_orphan(uint64_t task, uint32_t index, mr_ntfs_orphan *out);
/* "<código>\n<nombre>\n<descripción>" */
mr_status mr_ntfs_tree_orphan_text(uint64_t task, uint32_t index,
                                   uint8_t *buf, size_t cap, size_t *out_len);
/* Flujo $DATA (stream vacío = principal) como origen de sectores de 512 B; se cierra con
 * mr_source_close. Los tramos no fiables se leen como sectores ilegibles. */
mr_status mr_ntfs_open_stream(uint64_t volume, uint64_t record,
                              const uint8_t *stream, size_t stream_len, uint64_t *out_source);
/* Copia como tarea (progreso en bytes). Nunca el origen ni un dispositivo ni un directorio;
 * salida existente: MR_ERR_ALREADY_EXISTS salvo MR_NTFS_COPY_OVERWRITE; escritura atómica. */
mr_status mr_ntfs_copy_start(uint64_t volume, uint64_t record,
                             const uint8_t *stream, size_t stream_len,
                             const uint8_t *out_path, size_t out_path_len,
                             uint32_t flags, uint64_t *out_task);
mr_status mr_ntfs_copy_result(uint64_t task, mr_ntfs_copy_report *out);

/*
 * VMDK con descriptor separado (M1.5, issue #11, ADR-018): monolithicFlat, twoGbMaxExtentFlat,
 * twoGbMaxExtentSparse y descriptor que cita un único fichero SPARSE. mr_vmdk_extents_list
 * interpreta el descriptor SIN abrir ninguna extensión y devuelve un handle que se consulta por
 * índice y se libera con mr_vmdk_extents_free. mr_source_open_vmdk abre el disco usando SOLO los
 * ficheros de la lista files (nombre declarado en el descriptor, coincidencia exacta → ruta elegida
 * por el usuario); el motor nunca busca ni abre otros ficheros. Una extensión necesaria sin fichero
 * aprobado: MR_ERR_EXTERNAL_REFERENCE; nombre del descriptor que no es un nombre de fichero simple
 * (ruta absoluta, UNC, "..", separadores, dispositivo): MR_ERR_UNSUPPORTED_FEATURE aunque se
 * apruebe; lista mal formada (nombre repetido, dos nombres con el mismo fichero, nombre que no
 * corresponde a una extensión necesaria, el propio descriptor): MR_ERR_INVALID_ARGUMENT. Las
 * extensiones NOACCESS se leen como ilegibles; las ZERO, como dispersas.
 */
#define MR_VMDK_HAS_PARENT          1u   /* mr_vmdk_extents_info.flags */
#define MR_VMDK_OPENABLE            2u
#define MR_VMDK_ISSUE_NONE          0u   /* issue_kind */
#define MR_VMDK_ISSUE_CORRUPT       1u
#define MR_VMDK_ISSUE_UNSUPPORTED   2u
#define MR_VMDK_ISSUE_PARENT        3u   /* disco padre: fuera del alcance */
#define MR_VMDK_ACCESS_RW           1u   /* mr_vmdk_extent.access */
#define MR_VMDK_ACCESS_RDONLY       2u
#define MR_VMDK_ACCESS_NOACCESS     3u
#define MR_VMDK_EXTENT_SPARSE       1u   /* mr_vmdk_extent.kind */
#define MR_VMDK_EXTENT_FLAT         2u
#define MR_VMDK_EXTENT_ZERO         3u
#define MR_VMDK_EXTENT_VMFS         4u
#define MR_VMDK_EXTENT_VMFSSPARSE   5u
#define MR_VMDK_EXTENT_VMFSRDM      6u
#define MR_VMDK_EXTENT_VMFSRAW      7u
#define MR_VMDK_EXTENT_SESPARSE     8u
#define MR_VMDK_EXTENT_NEEDS_FILE      1u   /* mr_vmdk_extent.flags: hay que aprobar un fichero */
#define MR_VMDK_EXTENT_HAS_FILE_NAME   2u   /* la línea declara un nombre de fichero */
#define MR_MAX_APPROVED_FILES 32768u

typedef struct mr_vmdk_extents_info {  /* 32 bytes */
    uint32_t struct_size;        /* = sizeof(mr_vmdk_extents_info), lo escribe el llamador */
    uint32_t extent_count;
    uint32_t required_count;     /* extensiones para las que hay que aprobar un fichero */
    uint32_t flags;              /* MR_VMDK_HAS_PARENT | MR_VMDK_OPENABLE */
    uint64_t virtual_size;       /* bytes (suma de las extensiones) */
    uint32_t issue_kind;         /* primer problema (MR_VMDK_ISSUE_*) */
    uint32_t reserved;
} mr_vmdk_extents_info;

typedef struct mr_vmdk_extent {  /* 48 bytes */
    uint32_t struct_size;        /* = sizeof(mr_vmdk_extent), lo escribe el llamador */
    uint32_t access;             /* MR_VMDK_ACCESS_* */
    uint32_t kind;               /* MR_VMDK_EXTENT_* */
    uint32_t flags;              /* MR_VMDK_EXTENT_NEEDS_FILE | MR_VMDK_EXTENT_HAS_FILE_NAME */
    uint64_t sectors;            /* sectores de 512 bytes */
    uint64_t offset;             /* sectores dentro del fichero (FLAT) */
    uint64_t start_sector;       /* primer sector virtual */
    uint32_t issue_kind;         /* MR_VMDK_ISSUE_* de esta extensión */
    uint32_t reserved;
} mr_vmdk_extent;

typedef struct mr_approved_file {  /* 40 bytes en 64 bits */
    uint32_t struct_size;        /* = sizeof(mr_approved_file), lo escribe el llamador */
    uint32_t reserved;           /* debe ser 0 */
    const uint8_t *name;         /* nombre declarado (UTF-8, coincidencia exacta) */
    size_t name_len;
    const uint8_t *path;         /* fichero elegido por el usuario (UTF-8) */
    size_t path_len;
} mr_approved_file;

mr_status mr_vmdk_extents_list(const uint8_t *path, size_t path_len, uint64_t *out_list);
mr_status mr_vmdk_extents_info(uint64_t list, mr_vmdk_extents_info *out);
/* Tipo de disco y extensiones por tipo (texto). */
mr_status mr_vmdk_extents_describe(uint64_t list, uint8_t *buf, size_t cap, size_t *out_len);
/* Primer problema que impide abrir el disco (cadena vacía si no hay ninguno). */
mr_status mr_vmdk_extents_issue(uint64_t list, uint8_t *buf, size_t cap, size_t *out_len);
mr_status mr_vmdk_extent_get(uint64_t list, uint32_t index, mr_vmdk_extent *out);
/* Nombre exactamente como lo declara el descriptor (cadena vacía en ZERO). */
mr_status mr_vmdk_extent_name(uint64_t list, uint32_t index,
                              uint8_t *buf, size_t cap, size_t *out_len);
mr_status mr_vmdk_extent_issue(uint64_t list, uint32_t index,
                               uint8_t *buf, size_t cap, size_t *out_len);
mr_status mr_vmdk_extents_free(uint64_t list);
/* flags: 0 o MR_OPEN_TOLERANT. files puede ser NULL con count = 0. Se cierra con mr_source_close. */
mr_status mr_source_open_vmdk(const uint8_t *path, size_t path_len, uint32_t sector_size,
                              uint32_t flags, const mr_approved_file *files, uint32_t count,
                              uint64_t *out_source);

mr_status mr_task_status_get(uint64_t task, mr_task_status *out);
mr_status mr_task_cancel(uint64_t task);
mr_status mr_task_free(uint64_t task);

#ifdef __cplusplus
}
#endif
#endif /* MEMRESCUE_CORE_H */
