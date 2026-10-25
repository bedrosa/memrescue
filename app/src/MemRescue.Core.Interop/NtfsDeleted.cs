using System.Runtime.InteropServices;

namespace MemRescue.Core.Interop;

/// <summary>Recuperabilidad del flujo principal de un borrado (<c>MR_NTFS_RECOVERABLE_*</c>).</summary>
public enum NtfsRecoverability : uint
{
    /// <summary>Directorio o sin <c>$DATA</c>.</summary>
    NotApplicable = 0,
    Full = 1,
    Partial = 2,
    None = 3,
}

/// <summary>Fuentes que encontraron un borrado (<c>MR_NTFS_ORIGIN_*</c>).</summary>
[Flags]
public enum NtfsOrigins : uint
{
    Unknown = 0,
    /// <summary>Registro MFT no en uso.</summary>
    Mft = 1,
    /// <summary>Entrada caducada de la parte en uso de un índice vivo.</summary>
    IndexStale = 2,
    /// <summary>Espacio libre de un nodo del índice.</summary>
    IndexSlack = 4,
    /// <summary>Nodo del índice libre según <c>$BITMAP</c>.</summary>
    IndexFreeNode = 8,
    /// <summary>Índice de un directorio borrado.</summary>
    DeletedDirectoryIndex = 16,
}

/// <summary>Estado del directorio padre de un borrado (<c>MR_NTFS_PARENT_*</c>).</summary>
public enum NtfsParentState : uint
{
    Unknown = 0,
    Live = 1,
    Deleted = 2,
    Orphan = 3,
}

/// <summary>Estado de un tramo de clústeres de un borrado (<c>MR_NTFS_CLUSTER_*</c>).</summary>
public enum NtfsClusterState : uint
{
    Unknown = 0,
    Free = 1,
    OwnedByLiveFile = 2,
    Allocated = 3,
    AllocationUnknown = 4,
    SharedWithDeleted = 5,
}

public static class NtfsDeletedLabels
{
    public static string Label(this NtfsRecoverability r) => r switch
    {
        NtfsRecoverability.Full => "íntegro",
        NtfsRecoverability.Partial => "PARCIAL",
        NtfsRecoverability.None => "NINGUNO",
        _ => "-",
    };
}

/// <summary>
/// Fichero o directorio borrado (de la MFT, de los índices o de ambos). <see cref="NtfsEntry.State"/> es
/// <see cref="NtfsEntryState.Deleted"/> o uno de los estados solo residuales (nunca recuperables).
/// </summary>
public sealed record NtfsDeletedEntry(
    NtfsEntry Entry, string Path, string Summary, string OriginsText, NtfsRecoverability Recoverability,
    NtfsOrigins Origins, NtfsParentState ParentState, uint StreamCount, ulong Clusters, ulong FreeClusters,
    ulong PossiblyOverwrittenClusters)
{
    /// <summary><c>true</c> si tiene el registro de la MFT con sus atributos (se puede recuperar).</summary>
    public bool HasRecord => Entry.State == NtfsEntryState.Deleted;

    /// <summary><c>true</c> si se puede intentar recuperar su contenido (fichero con registro).</summary>
    public bool CanRecover => HasRecord && !Entry.IsDirectory;
}

/// <summary>Resultado de la búsqueda de borrados.</summary>
public sealed record NtfsDeletedScan(
    ulong Records, IReadOnlyList<NtfsDeletedEntry> Entries, ulong ResidualOnlyCount, ulong ProblemCount,
    ulong IndexDirectories, ulong IndexNodes, ulong IndexNodesUnreadable, ulong IndexFreeNodes,
    ulong ResidualAccepted, ulong ResidualRejected, uint Flags)
{
    public bool BitmapUnusable => (Flags & 2) != 0;

    public bool IndexUnreadable => (Flags & 16) != 0;

    public bool MftUnreadable => (Flags & 32) != 0;

    public bool Truncated => (Flags & (1 | 4 | 8)) != 0;
}

/// <summary>Tramo de bytes leído de clústeres que pueden haberse reutilizado.</summary>
public sealed record NtfsSuspectRange(
    ulong Offset, ulong Length, ulong Lcn, ulong Clusters, NtfsClusterState State, ulong? Owner, string Description);

/// <summary>Informe de una recuperación.</summary>
public sealed record NtfsRecoveryReport(
    ulong Bytes, ulong UntrustedBytes, ulong PossiblyOverwrittenBytes, ulong FilledBytes,
    NtfsRecoverability Recoverability, uint Sequence, bool RangesTruncated, string Summary,
    IReadOnlyList<NtfsSuspectRange> Suspect)
{
    /// <summary><c>true</c> si todos los bytes son datos fiables.</summary>
    public bool AllTrustworthy => UntrustedBytes == 0;
}

public sealed partial class NtfsVolume
{
    /// <summary>Lanza la búsqueda de borrados (MFT y entradas residuales de los índices).</summary>
    public NtfsDeletedTask StartDeletedScan()
    {
        Engine.Check(NativeMethods.mr_ntfs_deleted_start(Handle, out var h));
        return new NtfsDeletedTask(h);
    }

    /// <summary>
    /// Recupera el flujo <paramref name="stream"/> del borrado <paramref name="record"/> con la secuencia de
    /// su entrada en <paramref name="outputPath"/>. Un registro en uso, con otra secuencia o libre (entradas
    /// solo residuales) se rechaza con <see cref="EngineStatus.InvalidArgument"/>; si la salida existe,
    /// <see cref="EngineStatus.AlreadyExists"/> salvo con <paramref name="overwrite"/>; escritura atómica.
    /// </summary>
    public NtfsRecoverTask StartRecover(ulong record, uint sequence, string outputPath, string stream = "", bool overwrite = false)
    {
        var s = Engine.Utf8(stream);
        var o = Engine.Utf8(outputPath);
        Engine.Check(NativeMethods.mr_ntfs_recover_start(Handle, record, sequence, s, (nuint)s.Length, o, (nuint)o.Length,
            overwrite ? 1u : 0u, out var h));
        return new NtfsRecoverTask(h);
    }
}

/// <summary>Búsqueda de borrados en segundo plano.</summary>
public sealed class NtfsDeletedTask : EngineTask
{
    internal NtfsDeletedTask(ulong handle) : base(handle)
    {
    }

    public NtfsDeletedScan GetResult()
    {
        var i = new MrNtfsDeletedInfo { StructSize = (uint)Marshal.SizeOf<MrNtfsDeletedInfo>() };
        Engine.Check(NativeMethods.mr_ntfs_deleted_result(Handle, ref i));
        var entries = new List<NtfsDeletedEntry>((int)Math.Min(i.EntryCount, 1_000_000));
        for (ulong k = 0; k < i.EntryCount; k++)
        {
            var e = NtfsEntry.Empty();
            Engine.Check(NativeMethods.mr_ntfs_deleted_entry(Handle, k, ref e));
            var d = new MrNtfsDeletedDetail { StructSize = (uint)Marshal.SizeOf<MrNtfsDeletedDetail>() };
            Engine.Check(NativeMethods.mr_ntfs_deleted_detail(Handle, k, ref d));
            var index = k;
            string Text(uint field) =>
                Engine.ReadString((b, c) => (NativeMethods.mr_ntfs_deleted_text(Handle, index, field, b, c, out var n), n));
            entries.Add(new NtfsDeletedEntry(NtfsEntry.From(Text(0), e), Text(1), Text(2), Text(3),
                (NtfsRecoverability)d.Recoverability, (NtfsOrigins)d.Origins, (NtfsParentState)d.ParentState,
                d.StreamCount, d.Clusters, d.FreeClusters, d.PossiblyOverwrittenClusters));
        }
        return new NtfsDeletedScan(i.Records, entries, i.ResidualOnlyCount, i.ProblemCount, i.IndexDirectories,
            i.IndexNodes, i.IndexNodesUnreadable, i.IndexFreeNodes, i.ResidualAccepted, i.ResidualRejected, i.Flags);
    }
}

/// <summary>Recuperación de un borrado en segundo plano.</summary>
public sealed class NtfsRecoverTask : EngineTask
{
    internal NtfsRecoverTask(ulong handle) : base(handle)
    {
    }

    public NtfsRecoveryReport GetReport()
    {
        var r = new MrNtfsRecoverReport { StructSize = (uint)Marshal.SizeOf<MrNtfsRecoverReport>() };
        Engine.Check(NativeMethods.mr_ntfs_recover_result(Handle, ref r));
        var summary = Engine.ReadString((b, c) => (NativeMethods.mr_ntfs_recover_text(Handle, uint.MaxValue, b, c, out var n), n));
        var suspect = new List<NtfsSuspectRange>((int)r.SuspectCount);
        for (uint k = 0; k < r.SuspectCount; k++)
        {
            var x = new MrNtfsSuspectRange { StructSize = (uint)Marshal.SizeOf<MrNtfsSuspectRange>() };
            Engine.Check(NativeMethods.mr_ntfs_recover_suspect(Handle, k, ref x));
            var index = k;
            var what = Engine.ReadString((b, c) => (NativeMethods.mr_ntfs_recover_text(Handle, index, b, c, out var n), n));
            suspect.Add(new NtfsSuspectRange(x.Offset, x.Length, x.Lcn, x.Clusters, (NtfsClusterState)x.State,
                x.Owner == ulong.MaxValue ? null : x.Owner, what));
        }
        return new NtfsRecoveryReport(r.Bytes, r.UntrustedBytes, r.PossiblyOverwrittenBytes, r.FilledBytes,
            (NtfsRecoverability)r.Recoverability, r.Sequence, (r.Flags & 1) != 0, summary, suspect);
    }
}

internal static partial class NativeMethods
{
    [LibraryImport(Dll)]
    internal static partial int mr_ntfs_deleted_start(ulong volume, out ulong task);

    [LibraryImport(Dll)]
    internal static partial int mr_ntfs_deleted_result(ulong task, ref MrNtfsDeletedInfo info);

    [LibraryImport(Dll)]
    internal static partial int mr_ntfs_deleted_entry(ulong task, ulong index, ref MrNtfsEntry entry);

    [LibraryImport(Dll)]
    internal static partial int mr_ntfs_deleted_detail(ulong task, ulong index, ref MrNtfsDeletedDetail detail);

    [LibraryImport(Dll)]
    internal static partial int mr_ntfs_deleted_text(ulong task, ulong index, uint field, byte[]? buf, nuint cap, out nuint outLen);

    [LibraryImport(Dll)]
    internal static partial int mr_ntfs_recover_start(ulong volume, ulong record, uint sequence, byte[] stream, nuint streamLen,
        byte[] outPath, nuint outPathLen, uint flags, out ulong task);

    [LibraryImport(Dll)]
    internal static partial int mr_ntfs_recover_result(ulong task, ref MrNtfsRecoverReport report);

    [LibraryImport(Dll)]
    internal static partial int mr_ntfs_recover_suspect(ulong task, uint index, ref MrNtfsSuspectRange range);

    [LibraryImport(Dll)]
    internal static partial int mr_ntfs_recover_text(ulong task, uint index, byte[]? buf, nuint cap, out nuint outLen);
}

/// <summary>Espejo de <c>mr_ntfs_deleted_info</c> (88 bytes).</summary>
[StructLayout(LayoutKind.Sequential)]
public struct MrNtfsDeletedInfo
{
    public uint StructSize;
    public uint Flags;
    public ulong Records;
    public ulong EntryCount;
    public ulong ResidualOnlyCount;
    public ulong ProblemCount;
    public ulong IndexDirectories;
    public ulong IndexNodes;
    public ulong IndexNodesUnreadable;
    public ulong IndexFreeNodes;
    public ulong ResidualAccepted;
    public ulong ResidualRejected;
}

/// <summary>Espejo de <c>mr_ntfs_deleted_detail</c> (48 bytes).</summary>
[StructLayout(LayoutKind.Sequential)]
public struct MrNtfsDeletedDetail
{
    public uint StructSize;
    public uint Recoverability;
    public uint Origins;
    public uint ParentState;
    public uint StreamCount;
    public uint IndexRefCount;
    public ulong Clusters;
    public ulong FreeClusters;
    public ulong PossiblyOverwrittenClusters;
}

/// <summary>Espejo de <c>mr_ntfs_recover_report</c> (56 bytes).</summary>
[StructLayout(LayoutKind.Sequential)]
public struct MrNtfsRecoverReport
{
    public uint StructSize;
    public uint Flags;
    public ulong Bytes;
    public ulong UntrustedBytes;
    public ulong PossiblyOverwrittenBytes;
    public ulong FilledBytes;
    public uint Recoverability;
    public uint SuspectCount;
    public uint Sequence;
    public uint Reserved;
}

/// <summary>Espejo de <c>mr_ntfs_suspect_range</c> (48 bytes).</summary>
[StructLayout(LayoutKind.Sequential)]
public struct MrNtfsSuspectRange
{
    public uint StructSize;
    public uint State;
    public ulong Offset;
    public ulong Length;
    public ulong Lcn;
    public ulong Clusters;
    public ulong Owner;
}
