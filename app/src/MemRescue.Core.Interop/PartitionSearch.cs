using System.Runtime.InteropServices;

namespace MemRescue.Core.Interop;

/// <summary>Sistema de archivos de un volumen encontrado (<c>MR_FS_*</c>).</summary>
public enum FoundFileSystem : uint
{
    Unknown = 0,
    Ntfs = 1,
    Fat12 = 2,
    Fat16 = 3,
    Fat32 = 4,
    ExFat = 5,
}

/// <summary>Resultado de una comprobación de evidencia (<c>MR_CHECK_*</c>).</summary>
public enum EvidenceCheck : uint
{
    Unknown = 0,
    /// <summary>Presente y válida («+»).</summary>
    Yes = 1,
    /// <summary>Ausente o dañada: los sectores se leyeron («x»).</summary>
    No = 2,
    /// <summary>No verificable: algún sector necesario es ilegible y no se interpretó («?»).</summary>
    Unverifiable = 3,
    /// <summary>Fuera de la imagen («o»).</summary>
    Outside = 4,
    /// <summary>No aplica («-»).</summary>
    NotApplicable = 5,
}

/// <summary>Estado final de la búsqueda (<c>MR_SEARCH_*</c>).</summary>
public enum PartitionSearchStatus : uint
{
    Unknown = 0,
    Complete = 1,
    /// <summary>Presupuesto, cupo de un nivel o límite de volúmenes agotado: resultado parcial.</summary>
    BudgetExhausted = 2,
    /// <summary>Cancelada: resultado parcial.</summary>
    Cancelled = 3,
}

/// <summary>Familias de reconocedores (<c>MR_SEARCH_FAMILY_*</c>).</summary>
[Flags]
public enum SearchFamilies : uint
{
    /// <summary>Todas (valor por defecto).</summary>
    All = 0,
    Ntfs = 1,
    Fat = 2,
    ExFat = 4,
    Mbr = 8,
    Gpt = 16,
}

/// <summary>
/// Opciones de la búsqueda exprés (<c>mr_partition_search_options</c>). Un valor 0 toma el valor por
/// defecto documentado: 4096 lecturas, 32 MiB, 256 volúmenes, ventana de 8 KiB, rejillas de 64 y
/// 16 MiB, 64 cilindros y 8 candidatos por hueco.
/// </summary>
public sealed record PartitionSearchOptions
{
    public ulong MaxReads { get; init; }
    public ulong MaxBytes { get; init; }
    public uint MaxVolumes { get; init; }
    public uint ProbeWindow { get; init; }
    public ulong StartGridBytes { get; init; }
    public ulong EndGridBytes { get; init; }
    public uint Cylinders { get; init; }
    public uint ProbesPerGap { get; init; }
    public SearchFamilies Families { get; init; }
}

public static class PartitionSearchLabels
{
    /// <summary>Símbolo del indicador: + sí, x no, ? no verificable, o fuera de la imagen, - no aplica.</summary>
    public static char Symbol(this EvidenceCheck c) => c switch
    {
        EvidenceCheck.Yes => '+',
        EvidenceCheck.No => 'x',
        EvidenceCheck.Unverifiable => '?',
        EvidenceCheck.Outside => 'o',
        EvidenceCheck.NotApplicable => '-',
        _ => ' ',
    };

    public static string Label(this EvidenceCheck c) => c switch
    {
        EvidenceCheck.Yes => "sí",
        EvidenceCheck.No => "no o dañada",
        EvidenceCheck.Unverifiable => "NO VERIFICABLE (sector ilegible)",
        EvidenceCheck.Outside => "fuera de la imagen",
        EvidenceCheck.NotApplicable => "no aplica",
        _ => "desconocido",
    };

    public static string Label(this FoundFileSystem f) => f switch
    {
        FoundFileSystem.Ntfs => "NTFS",
        FoundFileSystem.Fat12 => "FAT12",
        FoundFileSystem.Fat16 => "FAT16",
        FoundFileSystem.Fat32 => "FAT32",
        FoundFileSystem.ExFat => "exFAT",
        _ => "desconocido",
    };

    public static string Label(this PartitionSearchStatus s) => s switch
    {
        PartitionSearchStatus.Complete => "completa",
        PartitionSearchStatus.BudgetExhausted => "presupuesto agotado (resultado parcial)",
        PartitionSearchStatus.Cancelled => "cancelada (resultado parcial)",
        _ => "desconocido",
    };
}

/// <summary>Volumen encontrado por la búsqueda, con sus indicadores y su confianza.</summary>
public sealed record FoundVolume(
    int Index, FoundFileSystem FileSystem, long StartByte, ulong LengthBytes, long StartLba, ulong SectorCount,
    uint FsSectorSize, ConfidenceLevel Confidence, EvidenceCheck Primary, EvidenceCheck Backup,
    EvidenceCheck Structures, EvidenceCheck Entry, EvidenceCheck SelfLocation, EvidenceCheck Aligned,
    int Tier, string Origin, IReadOnlyList<PartitionAnomaly> Anomalies)
{
    /// <summary>Número visible (desde 1), el mismo que usa la CLI con <c>--found</c>.</summary>
    public int Number => Index + 1;

    /// <summary>Empieza antes del inicio de la imagen: no se puede abrir.</summary>
    public bool Partial { get; init; }

    /// <summary>Una partición de la tabla del origen lo describe (<see cref="TableIndex"/>).</summary>
    public bool Listed { get; init; }

    /// <summary>Abrirlo exige pedir explícitamente que se aplique el desplazamiento detectado.</summary>
    public bool RequiresShift { get; init; }

    /// <summary>Índice de la partición de la tabla que lo describe (o <c>null</c>).</summary>
    public int? TableIndex { get; init; }

    public long PrimaryByte { get; init; }

    public long? BackupByte { get; init; }

    public ulong DeclaredStart { get; init; }

    public ulong Serial { get; init; }

    public byte SuggestedMbrType { get; init; }

    public long EndLba => StartLba + (long)SectorCount - 1;

    /// <summary>Indicadores compactos: E entrada, P principal, C copia, S estructuras, U autoubicación, A alineación.</summary>
    public string Indicators =>
        $"E{Entry.Symbol()} P{Primary.Symbol()} C{Backup.Symbol()} S{Structures.Symbol()} U{SelfLocation.Symbol()} A{Aligned.Symbol()}";

    /// <summary>«tabla #N», «parcial» o «encontrada».</summary>
    public string StateLabel => TableIndex is { } t ? $"tabla #{t + 1}" : Partial ? "parcial" : "encontrada";

    public string TierLabel => $"P{Tier}";
}

/// <summary>Tabla encontrada por la búsqueda fuera de la posición de la tabla principal.</summary>
public sealed record FoundTable(bool IsGpt, ulong Lba, ulong DeclaredLba, uint EntryCount);

/// <summary>Resultado materializado de una búsqueda.</summary>
public sealed record PartitionSearchResult(
    PartitionSearchStatus Status, IReadOnlyList<FoundVolume> Volumes, IReadOnlyList<FoundTable> Tables,
    IReadOnlyList<PartitionAnomaly> Anomalies, long? ShiftBytes, ulong Reads, ulong MaxReads, ulong BytesRead,
    ulong UnverifiableSectors, ulong Probed, ulong Pending, uint SectorSize, ulong Sectors, bool VolumeLimitReached);

/// <summary>
/// Tarea de búsqueda exprés de particiones perdidas. El progreso se expresa en lecturas respecto al
/// presupuesto (<see cref="TaskProgress.Total"/> = <c>max_reads</c>): la búsqueda no recorre el disco.
/// Cancelada, termina en <see cref="EngineTaskState.Cancelled"/> con resultado parcial.
/// </summary>
public sealed class PartitionSearchTask : EngineTask
{
    internal PartitionSearchTask(ulong handle, Source parent) : base(handle) => Parent = parent;

    /// <summary>Origen en que se busca.</summary>
    public Source Parent { get; }

    /// <summary>Resultado (la tarea debe haber terminado, también si se canceló).</summary>
    public PartitionSearchResult GetResult()
    {
        var info = new MrPartitionSearchInfo { StructSize = (uint)Marshal.SizeOf<MrPartitionSearchInfo>() };
        Engine.Check(NativeMethods.mr_partition_search_result(Handle, ref info));
        var volumes = new List<FoundVolume>((int)Math.Min(info.VolumeCount, 4096));
        for (uint i = 0; i < info.VolumeCount; i++)
        {
            var v = new MrFoundVolume { StructSize = (uint)Marshal.SizeOf<MrFoundVolume>() };
            Engine.Check(NativeMethods.mr_found_volume_get(Handle, i, ref v));
            var index = i;
            var origin = Engine.ReadString((b, c) => (NativeMethods.mr_found_volume_origin(Handle, index, b, c, out var n), n));
            volumes.Add(new FoundVolume((int)i, (FoundFileSystem)v.Fs, v.StartByte, v.LengthBytes, v.StartLba,
                v.SectorCount, v.FsSectorSize, (ConfidenceLevel)v.Confidence, (EvidenceCheck)v.Primary,
                (EvidenceCheck)v.Backup, (EvidenceCheck)v.Structures, (EvidenceCheck)v.Entry,
                (EvidenceCheck)v.SelfLocation, (EvidenceCheck)v.Aligned, (int)v.Tier, origin,
                ReadAnomalies(i, v.AnomalyCount))
            {
                Partial = (v.Flags & NativeMethods.FoundPartial) != 0,
                Listed = (v.Flags & NativeMethods.FoundListed) != 0,
                RequiresShift = (v.Flags & NativeMethods.FoundRequiresShift) != 0,
                TableIndex = v.TableIndex == NativeMethods.PartitionTableLevel ? null : (int)v.TableIndex,
                PrimaryByte = v.PrimaryByte,
                BackupByte = (v.Flags & NativeMethods.FoundHasBackup) != 0 ? v.BackupByte : null,
                DeclaredStart = v.DeclaredStart,
                Serial = v.Serial,
                SuggestedMbrType = (byte)v.SuggestedMbrType,
            });
        }
        var tables = new List<FoundTable>((int)Math.Min(info.TableCount, 4096));
        for (uint i = 0; i < info.TableCount; i++)
        {
            var t = new MrFoundTable { StructSize = (uint)Marshal.SizeOf<MrFoundTable>() };
            Engine.Check(NativeMethods.mr_found_table_get(Handle, i, ref t));
            tables.Add(new FoundTable(t.Kind == 2, t.Lba, t.DeclaredLba, t.EntryCount));
        }
        return new PartitionSearchResult((PartitionSearchStatus)info.Status, volumes, tables,
            ReadAnomalies(NativeMethods.PartitionTableLevel, info.AnomalyCount),
            (info.Flags & NativeMethods.SearchHasShift) != 0 ? info.ShiftBytes : null,
            info.Reads, info.MaxReads, info.BytesRead, info.UnverifiableSectors, info.Probed, info.Pending,
            info.SectorSize, info.Sectors, (info.Flags & NativeMethods.SearchVolumeLimit) != 0);
    }

    /// <summary>
    /// Abre el volumen encontrado como origen independiente. Un volumen parcial o desalineado se
    /// rechaza siempre; uno que depende del desplazamiento detectado solo con
    /// <paramref name="applyShift"/> (si no, <see cref="EngineStatus.InvalidArgument"/>).
    /// </summary>
    public Source OpenVolume(int index, bool applyShift = false)
    {
        ArgumentOutOfRangeException.ThrowIfNegative(index);
        Engine.Check(NativeMethods.mr_found_volume_open(Handle, (uint)index,
            applyShift ? NativeMethods.PartitionOpenApplyShift : 0, out var h));
        var v = new MrFoundVolume { StructSize = (uint)Marshal.SizeOf<MrFoundVolume>() };
        Engine.Check(NativeMethods.mr_found_volume_get(Handle, (uint)index, ref v));
        return new Source(h, Parent.Path, (ulong)v.StartLba);
    }

    private List<PartitionAnomaly> ReadAnomalies(uint volumeIndex, ulong count)
    {
        var list = new List<PartitionAnomaly>((int)Math.Min(count, 4096));
        for (uint i = 0; i < count; i++)
        {
            var a = new MrPartitionAnomaly { StructSize = (uint)Marshal.SizeOf<MrPartitionAnomaly>() };
            Engine.Check(NativeMethods.mr_found_volume_anomaly(Handle, volumeIndex, i, ref a));
            var index = i;
            var text = Engine.ReadString((b, c) =>
                (NativeMethods.mr_found_volume_anomaly_text(Handle, volumeIndex, index, b, c, out var n), n));
            var parts = text.Split('\n', 2);
            list.Add(new PartitionAnomaly(a.Code, parts[0], (AnomalySeverity)a.Severity, a.Lba, null,
                (a.Flags & NativeMethods.AnomalyHasValue) != 0 ? a.Value : null,
                parts.Length > 1 ? parts[1] : "", (a.Flags & NativeMethods.AnomalyValueSigned) != 0)
            {
                LbaSigned = (a.Flags & NativeMethods.AnomalyLbaSigned) != 0,
            });
        }
        return list;
    }
}

internal static partial class NativeMethods
{
    internal const uint SearchHasShift = 1;
    internal const uint SearchVolumeLimit = 2;
    internal const uint FoundPartial = 1;
    internal const uint FoundListed = 2;
    internal const uint FoundRequiresShift = 4;
    internal const uint FoundHasBackup = 8;
    internal const uint AnomalyLbaSigned = 8;

    [LibraryImport(Dll)]
    internal static partial int mr_source_partition_search_start(ulong source, in MrPartitionSearchOptions options, out ulong task);

    [LibraryImport(Dll)]
    internal static partial int mr_partition_search_result(ulong task, ref MrPartitionSearchInfo info);

    [LibraryImport(Dll)]
    internal static partial int mr_found_volume_get(ulong task, uint index, ref MrFoundVolume volume);

    [LibraryImport(Dll)]
    internal static partial int mr_found_volume_origin(ulong task, uint index, byte[]? buf, nuint cap, out nuint outLen);

    [LibraryImport(Dll)]
    internal static partial int mr_found_volume_anomaly(ulong task, uint volumeIndex, uint anomalyIndex, ref MrPartitionAnomaly anomaly);

    [LibraryImport(Dll)]
    internal static partial int mr_found_volume_anomaly_text(ulong task, uint volumeIndex, uint anomalyIndex, byte[]? buf, nuint cap, out nuint outLen);

    [LibraryImport(Dll)]
    internal static partial int mr_found_table_get(ulong task, uint index, ref MrFoundTable table);

    [LibraryImport(Dll)]
    internal static partial int mr_found_volume_open(ulong task, uint index, uint flags, out ulong source);
}

/// <summary>Espejo de <c>mr_partition_search_options</c> (64 bytes).</summary>
[StructLayout(LayoutKind.Sequential)]
public struct MrPartitionSearchOptions
{
    public uint StructSize;
    public uint Families;
    public ulong MaxReads;
    public ulong MaxBytes;
    public ulong StartGridBytes;
    public ulong EndGridBytes;
    public uint MaxVolumes;
    public uint ProbeWindow;
    public uint Cylinders;
    public uint ProbesPerGap;
    public uint Flags;
    public uint Reserved;
}

/// <summary>Espejo de <c>mr_partition_search_info</c> (96 bytes).</summary>
[StructLayout(LayoutKind.Sequential)]
public struct MrPartitionSearchInfo
{
    public uint StructSize;
    public uint Status;
    public uint VolumeCount;
    public uint TableCount;
    public uint AnomalyCount;
    public uint Flags;
    public long ShiftBytes;
    public ulong Reads;
    public ulong MaxReads;
    public ulong BytesRead;
    public ulong UnverifiableSectors;
    public ulong Probed;
    public ulong Pending;
    public uint SectorSize;
    public uint Reserved;
    public ulong Sectors;
}

/// <summary>Espejo de <c>mr_found_volume</c> (136 bytes).</summary>
[StructLayout(LayoutKind.Sequential)]
public struct MrFoundVolume
{
    public uint StructSize;
    public uint Fs;
    public long StartByte;
    public ulong LengthBytes;
    public long StartLba;
    public ulong SectorCount;
    public uint FsSectorSize;
    public uint Confidence;
    public uint Primary;
    public uint Backup;
    public uint Structures;
    public uint Entry;
    public uint SelfLocation;
    public uint Aligned;
    public uint Tier;
    public uint Flags;
    public uint TableIndex;
    public uint FoundTableIndex;
    public uint AnomalyCount;
    public uint SuggestedMbrType;
    public long PrimaryByte;
    public long BackupByte;
    public ulong DeclaredStart;
    public ulong Serial;
    public uint Score;
    public uint Reserved;
}

/// <summary>Espejo de <c>mr_found_table</c> (32 bytes).</summary>
[StructLayout(LayoutKind.Sequential)]
public struct MrFoundTable
{
    public uint StructSize;
    public uint Kind;
    public ulong Lba;
    public ulong DeclaredLba;
    public uint EntryCount;
    public uint Reserved;
}
