using System.Runtime.InteropServices;
using System.Text;
using MemRescue.Core.Interop;
using Xunit;

namespace MemRescue.Core.Interop.Tests;

/// <summary>Tablas de particiones a través de la DLL real (issue #12).</summary>
public sealed class PartitionTests : IDisposable
{
    private readonly string _dir = Directory.CreateTempSubdirectory("mr-interop-part-").FullName;

    public void Dispose()
    {
        GC.Collect();
        GC.WaitForPendingFinalizers();
        Directory.Delete(_dir, recursive: true);
    }

    /// <summary>
    /// Disco de 4096 sectores (cada byte = LBA mod 256 fuera de las tablas): primaria activa 0x07 en
    /// [2048, +1024), extendida 0x0F en [3072, +1024) con una lógica Linux en 3136 y una lógica de tipo
    /// desconocido 0x7F que termina fuera del disco.
    /// </summary>
    private string PartitionedImage()
    {
        var d = new byte[4096 * 512];
        for (var i = 0; i < d.Length; i++)
        {
            d[i] = (byte)(i / 512);
        }
        void Table(int lba, params (byte Boot, byte Kind, uint Start, uint Count)[] entries)
        {
            var s = d.AsSpan(lba * 512, 512);
            s.Clear();
            for (var i = 0; i < entries.Length; i++)
            {
                var e = s.Slice(0x1BE + 16 * i, 16);
                e[0] = entries[i].Boot;
                e[4] = entries[i].Kind;
                BitConverter.TryWriteBytes(e.Slice(8, 4), entries[i].Start);
                BitConverter.TryWriteBytes(e.Slice(12, 4), entries[i].Count);
            }
            s[510] = 0x55;
            s[511] = 0xAA;
        }
        Table(0, (0x80, 0x07, 2048, 1024), (0, 0x0F, 3072, 1024));
        Table(3072, (0, 0x83, 64, 512), (0, 0x05, 640, 384));
        Table(3712, (0, 0x7F, 64, 900));
        var p = Path.Combine(_dir, "particiones.img");
        File.WriteAllBytes(p, d);
        return p;
    }

    [Fact]
    public void StructLayoutsMatchHeader()
    {
        Assert.Equal(80, Marshal.SizeOf<MrPartitionTableInfo>());
        Assert.Equal(40, Marshal.OffsetOf<MrPartitionTableInfo>(nameof(MrPartitionTableInfo.Shift)).ToInt32());
        Assert.Equal(64, Marshal.OffsetOf<MrPartitionTableInfo>(nameof(MrPartitionTableInfo.DiskGuid)).ToInt32());
        Assert.Equal(96, Marshal.SizeOf<MrPartitionInfo>());
        Assert.Equal(72, Marshal.OffsetOf<MrPartitionInfo>(nameof(MrPartitionInfo.UniqueGuid)).ToInt32());
        Assert.Equal(88, Marshal.OffsetOf<MrPartitionInfo>(nameof(MrPartitionInfo.Attributes)).ToInt32());
        Assert.Equal(56, Marshal.OffsetOf<MrPartitionInfo>(nameof(MrPartitionInfo.GptType)).ToInt32());
        Assert.Equal(40, Marshal.SizeOf<MrPartitionAnomaly>());
    }

    [Fact]
    public void PartitionsAreListedWithConfidenceAndAnomalies()
    {
        var img = PartitionedImage();
        var before = File.ReadAllBytes(img);
        using var src = Engine.OpenSource(img);
        using var t = src.ReadPartitions();
        Assert.Equal(PartitionScheme.Mbr, t.Scheme);
        Assert.Equal(512u, t.SectorSize);
        Assert.Equal(ConfidenceLevel.Suspicious, t.Confidence);
        Assert.Equal(3, t.Partitions.Count);

        var p1 = t.Partitions[0];
        Assert.Equal((PartitionOrigin.Primary, 0u, 2048ul, 1024ul, 3071ul, (byte)0x07, true, ConfidenceLevel.Valid),
            (p1.Origin, p1.Slot, p1.StartLba, p1.SectorCount, p1.EndLba, p1.MbrType, p1.Active, p1.Confidence));
        Assert.Equal("NTFS / exFAT / HPFS", p1.TypeName);
        Assert.Equal("0x07 NTFS / exFAT / HPFS", p1.TypeLabel);
        Assert.Empty(p1.Anomalies);
        Assert.Equal(1, p1.Number);

        var l1 = t.Partitions[1];
        Assert.Equal((PartitionOrigin.Logical, 3072ul, 3136ul, "lógica (EBR 3072)"), (l1.Origin, l1.EbrLba, l1.StartLba, l1.OriginLabel));

        var l2 = t.Partitions[2];
        Assert.Equal(ConfidenceLevel.Suspicious, l2.Confidence);
        Assert.Equal("", l2.TypeName);
        Assert.Equal("0x7F desconocido", l2.TypeLabel);
        var names = l2.Anomalies.Select(a => a.Name).Order().ToList();
        Assert.Equal(["ebr_logical_outside_extended", "mbr_partially_outside_disk", "mbr_unknown_type"], names);
        var unknown = l2.Anomalies.Single(a => a.Name == "mbr_unknown_type");
        Assert.Equal((109u, AnomalySeverity.Suspicious, 3712ul, (ulong?)0x7F, (ulong?)null),
            (unknown.Code, unknown.Severity, unknown.Lba, unknown.Value, unknown.TargetLba));
        Assert.Contains("desconocido", unknown.Message, StringComparison.Ordinal);
        Assert.Empty(t.Anomalies);
        Assert.Equal(3, t.AllAnomalies.Count());

        // Abrir una partición: la LBA 1 es la 2049 del disco.
        using (var part = t.OpenPartition(0))
        {
            Assert.Equal(1024ul, part.Info.SectorCount);
            Assert.Equal(2048ul, part.RangeStart);
            Assert.StartsWith("Partición LBA 2048–3071 (tipo 0x07)", part.Description, StringComparison.Ordinal);
            Assert.All(part.ReadSectors(1, 1), b => Assert.Equal((byte)(2049 % 256), b));
            Assert.Equal(EngineStatus.OutOfRange, Assert.Throws<EngineException>(() => part.ReadSectors(1024, 1)).Status);
        }
        // La que termina fuera del disco se abre recortada.
        using (var trimmed = t.OpenPartition(2))
        {
            Assert.Equal(4096ul - 3776, trimmed.Info.SectorCount);
            Assert.Contains("recortada", trimmed.Description, StringComparison.Ordinal);
        }
        Assert.Equal(EngineStatus.OutOfRange, Assert.Throws<EngineException>(() => t.OpenPartition(3)).Status);
        Assert.Equal(before, File.ReadAllBytes(img));
    }

    [Fact]
    public void TableOutlivesItsSourceAndDisposeIsIdempotent()
    {
        var img = PartitionedImage();
        var src = Engine.OpenSource(img);
        var t = src.ReadPartitions();
        src.Dispose();
        using (var part = t.OpenPartition(1))
        {
            Assert.Equal(512ul, part.Info.SectorCount);
        }
        t.Dispose();
        t.Dispose();
        Assert.True(t.IsDisposed);
        Assert.Throws<ObjectDisposedException>(() => t.OpenPartition(0));
    }

    [Fact]
    public void DiskWithoutTableReportsAnInformativeAnomaly()
    {
        var p = Path.Combine(_dir, "vacio.img");
        File.WriteAllBytes(p, new byte[64 * 512]);
        using var src = Engine.OpenSource(p);
        using var t = src.ReadPartitions();
        Assert.Equal(PartitionScheme.None, t.Scheme);
        Assert.Equal(ConfidenceLevel.Valid, t.Confidence);
        Assert.Empty(t.Partitions);
        var a = Assert.Single(t.Anomalies);
        Assert.Equal(("mbr_missing", 100u, AnomalySeverity.Notice), (a.Name, a.Code, a.Severity));
    }

    // ---------- Corpus ----------

    private static string RepoRoot()
    {
        var d = new DirectoryInfo(AppContext.BaseDirectory);
        while (d is not null && !File.Exists(Path.Combine(d.FullName, "global.json")))
        {
            d = d.Parent;
        }
        return d?.FullName ?? throw new InvalidOperationException("No se encontró la raíz del repositorio.");
    }

    public static TheoryData<string> Manifests()
    {
        var data = new TheoryData<string>();
        foreach (var f in Directory.GetFiles(Path.Combine(RepoRoot(), "tests", "corpus", "manifests"), "*.manifest.toml").Order())
        {
            data.Add(Path.GetFileName(f));
        }
        return data;
    }

    /// <summary>Claves de nivel superior y tablas <c>[[nombre]]</c> de un manifiesto TOML plano.</summary>
    private static (Dictionary<string, string> Top, List<(string Table, Dictionary<string, string> Keys)> Tables) Parse(string path)
    {
        var top = new Dictionary<string, string>();
        var tables = new List<(string, Dictionary<string, string>)>();
        Dictionary<string, string>? current = null;
        foreach (var raw in File.ReadLines(path))
        {
            var l = raw.Trim();
            if (l.Length == 0 || l.StartsWith('#'))
            {
                continue;
            }
            if (l.StartsWith("[[", StringComparison.Ordinal))
            {
                current = [];
                tables.Add((l.Trim('[', ']'), current));
                continue;
            }
            var kv = l.Split(" = ", 2);
            (current ?? top)[kv[0].Trim()] = kv[1].Trim().Trim('"');
        }
        return (top, tables);
    }

    /// <summary>
    /// La tabla de cada imagen del corpus, leída por la DLL, coincide con el manifiesto: esquema,
    /// particiones MBR (o entradas del híbrido) seguidas de las GPT (inicio, tamaño, tipo, nombre y GUID
    /// único), copia GPT en uso, desplazamiento y el conjunto de anomalías con código en el manifiesto
    /// (MBR y GPT unidas). <c>mbr_displaced</c> e <c>image_shifted</c> en imágenes sin GPT son de la
    /// búsqueda de particiones perdidas (#15). Cada partición se abre y empieza por su marcador; en una
    /// imagen desplazada, solo pidiendo aplicar el desplazamiento.
    /// </summary>
    [Theory]
    [Trait("Category", "Corpus")]
    [MemberData(nameof(Manifests))]
    public void CorpusTableMatchesManifest(string manifestName)
    {
        var root = RepoRoot();
        var (top, tables) = Parse(Path.Combine(root, "tests", "corpus", "manifests", manifestName));
        var img = Path.Combine(root, "tests", "corpus", "generated", top["file"]);
        Assert.True(File.Exists(img), $"Falta {img}: ejecute scripts/test.ps1 o el generador.");
        using var src = Engine.OpenSource(img, uint.Parse(top["sector_size"]));
        using var t = src.ReadPartitions();
        var scheme = top["scheme"];
        Assert.Equal(scheme switch
        {
            "mbr" => PartitionScheme.Mbr,
            "hybrid" => PartitionScheme.Hybrid,
            "gpt" => PartitionScheme.Gpt,
            _ => PartitionScheme.None,
        }, t.Scheme);

        IEnumerable<Dictionary<string, string>> Rows(string name) => tables.Where(x => x.Table == name).Select(x => x.Keys);
        var mbrExpected = scheme switch
        {
            "mbr" => Rows("partitions").ToList(),
            "hybrid" => Rows("hybrid_mbr_entries").Where(k => k["mbr_type"] != "238").ToList(),
            _ => [],
        };
        var gptExpected = scheme is "gpt" or "hybrid" ? Rows("partitions").ToList() : [];
        var mbrGot = t.Partitions.Where(p => p.Origin != PartitionOrigin.Gpt).ToList();
        var gptGot = t.Partitions.Where(p => p.Origin == PartitionOrigin.Gpt).ToList();
        Assert.Equal(
            mbrExpected.Select(p => $"{p["start_lba"]}/{p["sectors"]}/{p["mbr_type"]}/{p["active"]}").ToList(),
            mbrGot.Select(p => $"{p.StartLba}/{p.SectorCount}/{p.MbrType}/{(p.Active ? "true" : "false")}").ToList());
        Assert.Equal(
            gptExpected.Select(p => $"{p["start_lba"]}/{p["sectors"]}/{p["gpt_type"]}/{p["gpt_name"]}/{p["unique_guid"]}").ToList(),
            gptGot.Select(p => $"{p.StartLba}/{p.SectorCount}/{p.GptType}/{p.Name}/{p.UniqueGuid}").ToList());
        Assert.True(t.Partitions.Take(mbrGot.Count).All(p => p.Origin != PartitionOrigin.Gpt), "las entradas MBR van primero");

        if (top.TryGetValue("gpt_selected", out var selected))
        {
            Assert.True(t.HasGpt);
            Assert.Equal(selected, t.GptSelected switch
            {
                GptCopyInUse.Primary => "primary",
                GptCopyInUse.Backup => "backup",
                _ => "none",
            });
            Assert.Equal(long.Parse(top["leading_sectors_missing"]), t.Shift);
        }
        else
        {
            Assert.False(t.HasGpt);
        }

        string[] comparable =
        [
            "mbr_missing", "mbr_empty", "mbr_signature_missing", "mbr_multiple_active", "mbr_overlap", "mbr_unknown_type",
            "mbr_outside_disk", "ebr_loop", "ebr_link_outside_disk", "ebr_link_outside_extended", "hybrid_mbr",
            "protective_mbr_size_mismatch", "superfloppy", "image_shifted", "gpt_usable_range_outside_image",
            "gpt_primary_header_missing", "gpt_backup_header_missing", "gpt_primary_header_outside_image",
            "gpt_backup_header_outside_image", "gpt_primary_header_crc", "gpt_backup_header_crc",
            "gpt_primary_entries_crc", "gpt_backup_entries_crc", "gpt_primary_entries_outside_image",
            "gpt_backup_entries_outside_image",
        ];
        var want = Rows("anomalies")
            .Where(a => a["code"] != "mbr_displaced" && !(a["code"] == "image_shifted" && scheme == "none"))
            .Select(a => $"{a["code"]}@{a["lba"]}").Order().ToList();
        var got = t.AllAnomalies
            .Select(a =>
            {
                var name = a.Name switch
                {
                    "mbr_partially_outside_disk" => "mbr_outside_disk",
                    "gpt_primary_header_size" => "gpt_primary_header_crc",
                    "gpt_backup_header_size" => "gpt_backup_header_crc",
                    var n => n,
                };
                var lba = name.EndsWith("_outside_image", StringComparison.Ordinal) && name != "gpt_usable_range_outside_image"
                    ? (long)a.Value!.Value - t.Shift
                    : name == "image_shifted" ? -a.SignedValue!.Value : (long)a.Lba;
                return (Name: name, Lba: lba);
            })
            .Where(x => comparable.Contains(x.Name))
            .Select(x => $"{x.Name}@{x.Lba}").Order().ToList();
        Assert.Equal(want, got);

        // Cada partición que empieza dentro se abre y empieza por el marcador del generador.
        var markers = Rows("partitions").Select(p => p["marker"]).ToList();
        if (scheme == "mbr" && top["leading_sectors_missing"] == "0")
        {
            foreach (var (p, m) in mbrGot.Zip(markers))
            {
                using var part = t.OpenPartition(p.Index);
                Assert.Equal(m, Encoding.ASCII.GetString(part.ReadSectors(0, 1), 0, m.Length));
            }
        }
        foreach (var (p, m) in gptGot.Zip(markers))
        {
            var shifted = t.Shift != 0;
            if (shifted)
            {
                Assert.Equal(EngineStatus.InvalidArgument, Assert.Throws<EngineException>(() => t.OpenPartition(p.Index)).Status);
            }
            using var part = t.OpenPartition(p.Index, applyShift: shifted);
            if (shifted)
            {
                Assert.Contains("desplazamiento aplicado", part.Description, StringComparison.Ordinal);
            }
            Assert.Equal(m, Encoding.ASCII.GetString(part.ReadSectors(0, 1), 0, m.Length));
        }
    }
}
