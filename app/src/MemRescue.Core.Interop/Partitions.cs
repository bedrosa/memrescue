using System.Runtime.InteropServices;

namespace MemRescue.Core.Interop;

/// <summary>Esquema de particionado detectado (<c>MR_SCHEME_*</c>).</summary>
public enum PartitionScheme : uint
{
    /// <summary>Sin tabla reconocible.</summary>
    None = 0,
    /// <summary>MBR clásico (con o sin extendida).</summary>
    Mbr = 1,
    /// <summary>MBR protector de GPT.</summary>
    Gpt = 2,
    /// <summary>MBR híbrido (GPT y entradas MBR que describen particiones).</summary>
    Hybrid = 3,
}

/// <summary>Confianza de una tabla o partición (<c>MR_CONFIDENCE_*</c>).</summary>
public enum ConfidenceLevel : uint
{
    Unknown = 0,
    Valid = 1,
    ValidWithFixes = 2,
    Suspicious = 3,
    Invalid = 4,
}

/// <summary>Gravedad de una anomalía (<c>MR_SEVERITY_*</c>).</summary>
public enum AnomalySeverity : uint
{
    Unknown = 0,
    Notice = 1,
    Suspicious = 2,
    Invalid = 3,
    /// <summary>Daño compensado por la otra copia GPT (confianza «válida con correcciones»).</summary>
    Compensated = 4,
}

/// <summary>Copia GPT en uso (<c>MR_GPT_*</c>).</summary>
public enum GptCopyInUse : uint
{
    /// <summary>Sin GPT o ninguna copia utilizable.</summary>
    None = 0,
    Primary = 1,
    /// <summary>Copia de respaldo en uso (la primaria no es utilizable).</summary>
    Backup = 2,
}

/// <summary>De qué tabla procede una partición (<c>MR_ORIGIN_*</c>).</summary>
public enum PartitionOrigin : uint
{
    Unknown = 0,
    Primary = 1,
    Logical = 2,
    Gpt = 3,
}

public static class PartitionLabels
{
    public static string Label(this ConfidenceLevel c) => c switch
    {
        ConfidenceLevel.Valid => "válida",
        ConfidenceLevel.ValidWithFixes => "válida con correcciones",
        ConfidenceLevel.Suspicious => "SOSPECHOSA",
        ConfidenceLevel.Invalid => "INVÁLIDA",
        _ => "desconocida",
    };

    public static string Label(this AnomalySeverity s) => s switch
    {
        AnomalySeverity.Notice => "aviso",
        AnomalySeverity.Suspicious => "sospechosa",
        AnomalySeverity.Invalid => "INVÁLIDA",
        AnomalySeverity.Compensated => "compensada",
        _ => "desconocida",
    };

    public static string Label(this PartitionScheme s) => s switch
    {
        PartitionScheme.None => "sin tabla de particiones reconocible",
        PartitionScheme.Mbr => "MBR",
        PartitionScheme.Gpt => "GPT",
        PartitionScheme.Hybrid => "híbrido MBR + GPT",
        _ => "desconocido",
    };
}

/// <summary>Anomalía de una tabla o partición.</summary>
/// <param name="Code">Código numérico estable (<c>MR_ANOMALY_*</c>).</param>
/// <param name="Name">Código textual estable (p. ej. <c>ebr_loop</c>).</param>
/// <param name="Severity">Gravedad.</param>
/// <param name="Lba">Sector de tabla donde se detectó.</param>
/// <param name="TargetLba">Destino (enlaces de EBR), si lo hay.</param>
/// <param name="Value">Valor asociado (tipo, tamaño declarado...), si lo hay.</param>
/// <param name="Message">Descripción legible.</param>
/// <param name="ValueSigned"><see cref="Value"/> es un entero con signo (desplazamiento de <c>image_shifted</c>).</param>
public sealed record PartitionAnomaly(uint Code, string Name, AnomalySeverity Severity, ulong Lba,
    ulong? TargetLba, ulong? Value, string Message, bool ValueSigned = false)
{
    /// <summary><see cref="Value"/> interpretado con signo.</summary>
    public long? SignedValue => Value is { } v ? unchecked((long)v) : null;

    public override string ToString() => $"[{Severity.Label()}] {Name}: {Message}";
}

/// <summary>Partición declarada por una tabla.</summary>
public sealed record PartitionEntry(
    int Index, PartitionOrigin Origin, uint Slot, ulong EbrLba, ulong StartLba, ulong SectorCount,
    byte MbrType, Guid GptType, bool Active, ConfidenceLevel Confidence, string TypeName,
    IReadOnlyList<PartitionAnomaly> Anomalies)
{
    /// <summary>Nombre GPT (vacío en MBR).</summary>
    public string Name { get; init; } = "";

    /// <summary>GUID único GPT (vacío en MBR).</summary>
    public Guid UniqueGuid { get; init; }

    /// <summary>Atributos GPT de 64 bits.</summary>
    public ulong Attributes { get; init; }

    /// <summary>Número visible (desde 1), el mismo que usa la CLI con <c>--partition</c>.</summary>
    public int Number => Index + 1;

    public ulong EndLba => SectorCount == 0 ? StartLba : StartLba + SectorCount - 1;

    public string OriginLabel => Origin switch
    {
        PartitionOrigin.Primary => $"primaria {Slot + 1}",
        PartitionOrigin.Logical => $"lógica (EBR {EbrLba})",
        PartitionOrigin.Gpt => $"GPT {Slot + 1}",
        _ => "?",
    };

    public string TypeLabel => Origin == PartitionOrigin.Gpt
        ? (TypeName.Length > 0 ? TypeName : $"desconocido ({GptType})")
        : $"0x{MbrType:X2} {(TypeName.Length > 0 ? TypeName : "desconocido")}";
}

/// <summary>
/// Tabla de particiones leída por el motor. Las listas se materializan al crearla; el handle se
/// conserva para abrir particiones como orígenes (<see cref="OpenPartition"/>).
/// </summary>
public sealed class PartitionTable : EngineHandle
{
    internal PartitionTable(ulong handle, Source parent) : base(handle)
    {
        Parent = parent;
        var info = new MrPartitionTableInfo { StructSize = (uint)Marshal.SizeOf<MrPartitionTableInfo>() };
        Engine.Check(NativeMethods.mr_partitions_info(Handle, ref info));
        Scheme = Enum.IsDefined(typeof(PartitionScheme), info.Scheme) ? (PartitionScheme)info.Scheme : PartitionScheme.None;
        SectorSize = info.SectorSize;
        Confidence = (ConfidenceLevel)info.Confidence;
        HasGpt = (info.GptFlags & NativeMethods.GptPresent) != 0;
        GptSelected = (GptCopyInUse)info.GptSelected;
        Shift = info.Shift;
        TrailingSectorsMissing = info.TrailingSectorsMissing;
        GptSectorSize = info.GptSectorSize;
        DiskGuid = info.DiskGuid;
        Anomalies = ReadAnomalies(NativeMethods.PartitionTableLevel, info.AnomalyCount);
        var parts = new List<PartitionEntry>((int)Math.Min(info.PartitionCount, 8192));
        for (uint i = 0; i < info.PartitionCount; i++)
        {
            var p = new MrPartitionInfo { StructSize = (uint)Marshal.SizeOf<MrPartitionInfo>() };
            Engine.Check(NativeMethods.mr_partition_get(Handle, i, ref p));
            var index = i;
            var name = Engine.ReadString((b, c) => (NativeMethods.mr_partition_type_name(Handle, index, b, c, out var n), n));
            var gptName = Engine.ReadString((b, c) => (NativeMethods.mr_partition_name(Handle, index, b, c, out var n), n));
            parts.Add(new PartitionEntry((int)i, (PartitionOrigin)p.Origin, p.Slot, p.EbrLba, p.StartLba, p.SectorCount,
                (byte)p.MbrType, p.GptType, p.Active != 0, (ConfidenceLevel)p.Confidence, name,
                ReadAnomalies(i, p.AnomalyCount))
            {
                Name = gptName,
                UniqueGuid = p.UniqueGuid,
                Attributes = p.Attributes,
            });
        }
        Partitions = parts;
    }

    /// <summary>Origen del que se leyó la tabla.</summary>
    public Source Parent { get; }

    public PartitionScheme Scheme { get; }

    public uint SectorSize { get; }

    /// <summary>Confianza del conjunto (peor gravedad de todas las anomalías).</summary>
    public ConfidenceLevel Confidence { get; }

    public IReadOnlyList<PartitionEntry> Partitions { get; }

    /// <summary>Anomalías de la tabla (no atribuibles a una sola partición).</summary>
    public IReadOnlyList<PartitionAnomaly> Anomalies { get; }

    /// <summary>Hay lectura GPT (esquema GPT o híbrido).</summary>
    public bool HasGpt { get; }

    /// <summary>Copia GPT en uso.</summary>
    public GptCopyInUse GptSelected { get; }

    /// <summary>
    /// Desplazamiento de la imagen: LBA del disco = LBA de la imagen + <see cref="Shift"/>. Las
    /// particiones GPT llevan LBA del disco; abrir una con desplazamiento exige pedirlo
    /// (<see cref="OpenPartition"/> con <c>applyShift</c>).
    /// </summary>
    public long Shift { get; }

    /// <summary>Sectores del disco que faltan al final de la imagen (según la GPT).</summary>
    public ulong TrailingSectorsMissing { get; }

    /// <summary>Tamaño de sector con que se interpretó la GPT (0 sin GPT).</summary>
    public uint GptSectorSize { get; }

    /// <summary>GUID de disco de la cabecera GPT elegida (vacío si no hay).</summary>
    public Guid DiskGuid { get; }

    /// <summary>Todas las anomalías: las de la tabla y las de cada partición.</summary>
    public IEnumerable<PartitionAnomaly> AllAnomalies => Anomalies.Concat(Partitions.SelectMany(p => p.Anomalies));

    /// <summary>
    /// Abre la partición como un origen independiente (vista acotada; si termina fuera del origen se
    /// abre la parte existente y la descripción lo indica). Una partición GPT de una imagen desplazada
    /// solo se abre con <paramref name="applyShift"/>: el motor calcula la LBA de la imagen con el
    /// desplazamiento informado y lo dice en la descripción; sin él, <see cref="EngineStatus.InvalidArgument"/>.
    /// </summary>
    public Source OpenPartition(int index, bool applyShift = false)
    {
        ArgumentOutOfRangeException.ThrowIfNegative(index);
        Engine.Check(NativeMethods.mr_partition_open_ex(Handle, (uint)index,
            applyShift ? NativeMethods.PartitionOpenApplyShift : 0, out var h));
        var start = index < Partitions.Count ? Partitions[index].StartLba : 0;
        if (applyShift && index < Partitions.Count && Partitions[index].Origin == PartitionOrigin.Gpt)
        {
            start = (ulong)((long)start - Shift);
        }
        return new Source(h, Parent.Path, start);
    }

    private List<PartitionAnomaly> ReadAnomalies(uint partitionIndex, ulong count)
    {
        var list = new List<PartitionAnomaly>((int)Math.Min(count, 4096));
        for (uint i = 0; i < count; i++)
        {
            var a = new MrPartitionAnomaly { StructSize = (uint)Marshal.SizeOf<MrPartitionAnomaly>() };
            Engine.Check(NativeMethods.mr_partition_anomaly(Handle, partitionIndex, i, ref a));
            var index = i;
            var text = Engine.ReadString((b, c) =>
                (NativeMethods.mr_partition_anomaly_text(Handle, partitionIndex, index, b, c, out var n), n));
            var parts = text.Split('\n', 2);
            list.Add(new PartitionAnomaly(a.Code, parts[0], (AnomalySeverity)a.Severity, a.Lba,
                (a.Flags & NativeMethods.AnomalyHasTarget) != 0 ? a.TargetLba : null,
                (a.Flags & NativeMethods.AnomalyHasValue) != 0 ? a.Value : null,
                parts.Length > 1 ? parts[1] : "", (a.Flags & NativeMethods.AnomalyValueSigned) != 0));
        }
        return list;
    }

    private protected override void Release(ulong handle)
    {
        if (handle != 0)
        {
            NativeMethods.mr_partitions_free(handle);
        }
    }
}

/// <summary>Espejo de <c>mr_partition_table_info</c> (80 bytes).</summary>
[StructLayout(LayoutKind.Sequential)]
public struct MrPartitionTableInfo
{
    public uint StructSize;
    public uint Scheme;
    public uint SectorSize;
    public uint Confidence;
    public ulong PartitionCount;
    public ulong AnomalyCount;
    public uint GptSelected;
    public uint GptFlags;
    public long Shift;
    public ulong TrailingSectorsMissing;
    public uint GptSectorSize;
    public uint Reserved;
    public Guid DiskGuid;
}

/// <summary>Espejo de <c>mr_partition_info</c> (96 bytes). <c>gpt_type</c> tiene el orden de bytes del disco, el mismo que <see cref="Guid"/>.</summary>
[StructLayout(LayoutKind.Sequential)]
public struct MrPartitionInfo
{
    public uint StructSize;
    public uint Origin;
    public uint Slot;
    public uint MbrType;
    public ulong EbrLba;
    public ulong StartLba;
    public ulong SectorCount;
    public uint Active;
    public uint Confidence;
    public uint AnomalyCount;
    public uint Reserved;
    public Guid GptType;
    public Guid UniqueGuid;
    public ulong Attributes;
}

/// <summary>Espejo de <c>mr_partition_anomaly</c> (40 bytes).</summary>
[StructLayout(LayoutKind.Sequential)]
public struct MrPartitionAnomaly
{
    public uint StructSize;
    public uint Code;
    public uint Severity;
    public uint Flags;
    public ulong Lba;
    public ulong TargetLba;
    public ulong Value;
}
