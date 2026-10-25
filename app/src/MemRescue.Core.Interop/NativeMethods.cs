using System.Runtime.InteropServices;

namespace MemRescue.Core.Interop;

/// <summary>
/// Declaraciones de la interfaz C de memrescue_core.dll (ABI 3).
/// Contrato completo en core/mr-ffi/include/memrescue_core.h y docs/API_FFI.md.
/// </summary>
internal static partial class NativeMethods
{
    internal const string Dll = "memrescue_core";
    internal const uint ExpectedAbiVersion = 3;

    internal const uint OpenForceRaw = 1;
    internal const uint OpenSplit = 2;
    internal const uint OpenTolerant = 4;
    internal const uint OpenIgnoreLinkMismatch = 8;
    internal const uint VerifyReadBack = 1;
    internal const uint VerifyIgnoreAllErrors = 2;
    internal const int MaxSkipCodes = 256;
    internal const uint ReadReadBack = 1;
    internal const uint PartitionTableLevel = 0xFFFF_FFFF;
    internal const uint AnomalyHasTarget = 1;
    internal const uint AnomalyHasValue = 2;
    internal const uint AnomalyValueSigned = 4;
    internal const uint GptPresent = 1;
    internal const uint PartitionOpenApplyShift = 1;
    internal const uint VmdkHasParent = 1;
    internal const uint VmdkOpenable = 2;
    internal const uint VmdkExtentNeedsFile = 1;

    [LibraryImport(Dll)]
    internal static partial uint mr_abi_version();

    [LibraryImport(Dll)]
    internal static partial int mr_engine_version(byte[]? buf, nuint cap, out nuint outLen);

    [LibraryImport(Dll)]
    internal static partial int mr_last_error(byte[]? buf, nuint cap, out nuint outLen);

    [LibraryImport(Dll)]
    internal static partial int mr_image_probe(byte[] path, nuint pathLen, out uint format);

    [LibraryImport(Dll)]
    internal static partial int mr_image_info_get(byte[] path, nuint pathLen, uint sectorSize, out MrImageInfo info);

    [LibraryImport(Dll)]
    internal static partial int mr_source_open(byte[] path, nuint pathLen, uint sectorSize, uint flags, out ulong source);

    [LibraryImport(Dll)]
    internal static partial int mr_source_open_ex(byte[] path, nuint pathLen, in MrOpenOptions options, out ulong source);

    [LibraryImport(Dll)]
    internal static partial int mr_image_external_hints(byte[] path, nuint pathLen, byte[]? buf, nuint cap, out nuint outLen, out uint count);

    [LibraryImport(Dll)]
    internal static partial int mr_source_info(ulong source, out MrImageInfo info);

    [LibraryImport(Dll)]
    internal static partial int mr_source_describe(ulong source, byte[]? buf, nuint cap, out nuint outLen);

    [LibraryImport(Dll)]
    internal static partial int mr_source_read(ulong source, ulong lba, uint count, byte[] buf, nuint cap, out nuint outLen);

    [LibraryImport(Dll)]
    internal static partial int mr_source_read_mapped(
        ulong source, ulong lba, uint count, uint retries,
        byte[] buf, nuint cap, byte[] states, nuint statesCap, out nuint outLen);

    [LibraryImport(Dll)]
    internal static partial int mr_source_read_mapped_ex(
        ulong source, ulong lba, uint count, in MrReadOptions options,
        byte[] buf, nuint cap, byte[] states, nuint statesCap, out nuint outLen);

    [LibraryImport(Dll)]
    internal static partial int mr_source_open_range(ulong source, ulong startLba, ulong count, out ulong rangeSource);

    [LibraryImport(Dll)]
    internal static partial int mr_source_close(ulong source);

    [LibraryImport(Dll)]
    internal static partial int mr_split_discover(byte[] path, nuint pathLen, byte[]? buf, nuint cap, out nuint outLen, out uint count);

    [LibraryImport(Dll)]
    internal static partial int mr_hash_start(
        byte[] path, nuint pathLen, uint sectorSize, uint algorithmsMask,
        byte[]? storePath, nuint storeLen, out ulong task);

    [LibraryImport(Dll)]
    internal static partial int mr_source_hash_start(ulong source, uint algorithmsMask, byte[]? storePath, nuint storeLen, out ulong task);

    [LibraryImport(Dll)]
    internal static partial int mr_hash_result(ulong task, uint algorithm, byte[]? buf, nuint cap, out nuint outLen);

    [LibraryImport(Dll)]
    internal static partial int mr_source_verify_start(
        ulong source, ulong startLba, ulong count, uint retries, ulong jumpSectors, uint flags, out ulong task);

    [LibraryImport(Dll)]
    internal static partial int mr_source_verify_start_ex(ulong source, in MrVerifyOptions options, out ulong task);

    [LibraryImport(Dll)]
    internal static partial int mr_verify_result(ulong task, out MrVerifyReport report);

    [LibraryImport(Dll)]
    internal static partial int mr_verify_error_summary(ulong task, out ulong omitted, out uint codeCount);

    [LibraryImport(Dll)]
    internal static partial int mr_verify_error_code(ulong task, uint index, out int code, out ulong sectors);

    [LibraryImport(Dll)]
    internal static partial int mr_verify_untrusted_range(ulong task, ulong index, out ulong lba, out ulong count, out uint state);

    [LibraryImport(Dll)]
    internal static partial int mr_source_partitions(ulong source, out ulong table);

    [LibraryImport(Dll)]
    internal static partial int mr_partitions_info(ulong table, ref MrPartitionTableInfo info);

    [LibraryImport(Dll)]
    internal static partial int mr_partition_get(ulong table, uint index, ref MrPartitionInfo info);

    [LibraryImport(Dll)]
    internal static partial int mr_partition_type_name(ulong table, uint index, byte[]? buf, nuint cap, out nuint outLen);

    [LibraryImport(Dll)]
    internal static partial int mr_partition_anomaly(ulong table, uint partitionIndex, uint anomalyIndex, ref MrPartitionAnomaly anomaly);

    [LibraryImport(Dll)]
    internal static partial int mr_partition_anomaly_text(ulong table, uint partitionIndex, uint anomalyIndex, byte[]? buf, nuint cap, out nuint outLen);

    [LibraryImport(Dll)]
    internal static partial int mr_partition_open(ulong table, uint index, out ulong source);

    [LibraryImport(Dll)]
    internal static partial int mr_partition_open_ex(ulong table, uint index, uint flags, out ulong source);

    [LibraryImport(Dll)]
    internal static partial int mr_partition_name(ulong table, uint index, byte[]? buf, nuint cap, out nuint outLen);

    [LibraryImport(Dll)]
    internal static partial int mr_partition_apm_type(ulong table, uint index, byte[]? buf, nuint cap, out nuint outLen);

    [LibraryImport(Dll)]
    internal static partial int mr_partitions_free(ulong table);

    // VMDK con descriptor separado (incidencia 11).
    [LibraryImport(Dll)]
    internal static partial int mr_vmdk_extents_list(byte[] path, nuint pathLen, out ulong list);

    [LibraryImport(Dll)]
    internal static partial int mr_vmdk_extents_info(ulong list, ref MrVmdkExtentsInfo info);

    [LibraryImport(Dll)]
    internal static partial int mr_vmdk_extents_describe(ulong list, byte[]? buf, nuint cap, out nuint outLen);

    [LibraryImport(Dll)]
    internal static partial int mr_vmdk_extents_issue(ulong list, byte[]? buf, nuint cap, out nuint outLen);

    [LibraryImport(Dll)]
    internal static partial int mr_vmdk_extent_get(ulong list, uint index, ref MrVmdkExtent extent);

    [LibraryImport(Dll)]
    internal static partial int mr_vmdk_extent_name(ulong list, uint index, byte[]? buf, nuint cap, out nuint outLen);

    [LibraryImport(Dll)]
    internal static partial int mr_vmdk_extent_issue(ulong list, uint index, byte[]? buf, nuint cap, out nuint outLen);

    [LibraryImport(Dll)]
    internal static partial int mr_vmdk_extents_free(ulong list);

    [LibraryImport(Dll)]
    internal static partial int mr_source_open_vmdk(byte[] path, nuint pathLen, uint sectorSize, uint flags,
        MrApprovedFile[] files, uint count, out ulong source);

    [LibraryImport(Dll)]
    internal static partial int mr_task_status_get(ulong task, out MrTaskStatus status);

    [LibraryImport(Dll)]
    internal static partial int mr_task_cancel(ulong task);

    [LibraryImport(Dll)]
    internal static partial int mr_task_free(ulong task);
}

/// <summary>Espejo de <c>mr_image_info</c> (32 bytes).</summary>
[StructLayout(LayoutKind.Sequential)]
public struct MrImageInfo
{
    public ulong SizeBytes;
    public ulong SectorCount;
    public ulong TrailingBytes;
    public uint SectorSize;
    public uint Format;
}

/// <summary>Espejo de <c>mr_str</c> (16 bytes): cadena UTF-8 de entrada fijada en memoria durante la llamada.</summary>
[StructLayout(LayoutKind.Sequential)]
public struct MrStr
{
    public nint Ptr;
    public nuint Len;
}

/// <summary>
/// Espejo de <c>mr_open_options</c> (32 bytes). <see cref="Externals"/> apunta a un <c>MrStr[]</c> fijado en
/// memoria durante la llamada.
/// </summary>
[StructLayout(LayoutKind.Sequential)]
public struct MrOpenOptions
{
    public uint StructSize;
    public uint Flags;
    public uint SectorSize;
    public uint ExternalCount;
    public nint Externals;
    public ulong Reserved;
}

/// <summary>Espejo de <c>mr_task_status</c> (24 bytes).</summary>
[StructLayout(LayoutKind.Sequential)]
public struct MrTaskStatus
{
    public uint State;
    public uint Reserved;
    public ulong Done;
    public ulong Total;
}

/// <summary>
/// Espejo de <c>mr_verify_options</c> (48 bytes). <see cref="SkipCodes"/> apunta a un
/// <c>int[]</c> fijado en memoria durante la llamada.
/// </summary>
[StructLayout(LayoutKind.Sequential)]
public struct MrVerifyOptions
{
    public uint StructSize;
    public uint Flags;
    public ulong StartLba;
    public ulong Count;
    public ulong JumpSectors;
    public uint Retries;
    public uint SkipCodeCount;
    public nint SkipCodes;
}

/// <summary>
/// Espejo de <c>mr_read_options</c> (32 bytes). <c>bad_fill</c>/<c>skip_fill</c> son <c>uint8_t[4]</c>:
/// se guardan como <see cref="uint"/> cuyo orden de bytes en memoria (little-endian, x64) es el del patrón.
/// </summary>
[StructLayout(LayoutKind.Sequential)]
public struct MrReadOptions
{
    public uint StructSize;
    public uint Flags;
    public ulong JumpSectors;
    public uint Retries;
    public uint BadFill;
    public uint SkipFill;
    public uint Reserved;
}

/// <summary>Espejo de <c>mr_verify_report</c> (72 bytes).</summary>
[StructLayout(LayoutKind.Sequential)]
public struct MrVerifyReport
{
    public ulong SectorsRequested;
    public ulong SectorsRead;
    public ulong SectorsRecovered;
    public ulong SectorsUnreadable;
    public ulong SectorsSkipped;
    public ulong SectorsSparse;
    public ulong RetryReads;
    public ulong UntrustedRangeCount;
    public uint RangesTruncated;
    public uint Reserved;
}
