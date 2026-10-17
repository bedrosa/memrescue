using System.Runtime.InteropServices;

namespace MemRescue.Core.Interop;

/// <summary>
/// Declaraciones de la interfaz C de memrescue_core.dll (ABI 2).
/// Contrato completo en core/mr-ffi/include/memrescue_core.h y docs/API_FFI.md.
/// </summary>
internal static partial class NativeMethods
{
    internal const string Dll = "memrescue_core";
    internal const uint ExpectedAbiVersion = 2;

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
    internal static partial int mr_source_info(ulong source, out MrImageInfo info);

    [LibraryImport(Dll)]
    internal static partial int mr_source_read(ulong source, ulong lba, uint count, byte[] buf, nuint cap, out nuint outLen);

    [LibraryImport(Dll)]
    internal static partial int mr_source_close(ulong source);

    [LibraryImport(Dll)]
    internal static partial int mr_hash_start(
        byte[] path, nuint pathLen, uint sectorSize, uint algorithmsMask,
        byte[]? storePath, nuint storeLen, out ulong task);

    [LibraryImport(Dll)]
    internal static partial int mr_source_hash_start(ulong source, uint algorithmsMask, byte[]? storePath, nuint storeLen, out ulong task);

    [LibraryImport(Dll)]
    internal static partial int mr_hash_result(ulong task, uint algorithm, byte[]? buf, nuint cap, out nuint outLen);

    [LibraryImport(Dll)]
    internal static partial int mr_source_verify_start(ulong source, ulong startLba, ulong count, uint retries, out ulong task);

    [LibraryImport(Dll)]
    internal static partial int mr_verify_result(ulong task, out MrVerifyReport report);

    [LibraryImport(Dll)]
    internal static partial int mr_verify_bad_range(ulong task, ulong index, out ulong lba, out ulong count);

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
    public uint Reserved;
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

/// <summary>Espejo de <c>mr_verify_report</c> (48 bytes).</summary>
[StructLayout(LayoutKind.Sequential)]
public struct MrVerifyReport
{
    public ulong SectorsRequested;
    public ulong SectorsOk;
    public ulong SectorsBad;
    public ulong RetryReads;
    public ulong BadRangeCount;
    public uint BadRangesTruncated;
    public uint Reserved;
}
