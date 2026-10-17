using System.Buffers.Binary;
using System.Diagnostics;
using System.IO;
using System.Reflection;
using System.Security.Cryptography;
using Xunit;

[assembly: CollectionBehavior(DisableTestParallelization = true)]

namespace MemRescue.UiTests;

/// <summary>Rutas del repositorio y del ejecutable que se prueba.</summary>
internal static class RepoPaths
{
    /// <summary>Raíz del repositorio: primer directorio ascendente que contiene <c>app/MemRescue.slnx</c>.</summary>
    public static string Root { get; } = FindRoot();

    private static string FindRoot()
    {
        for (var dir = new DirectoryInfo(AppContext.BaseDirectory); dir is not null; dir = dir.Parent)
        {
            if (File.Exists(Path.Combine(dir.FullName, "app", "MemRescue.slnx")))
            {
                return dir.FullName;
            }
        }
        throw new InvalidOperationException($"No se encuentra la raíz del repositorio desde {AppContext.BaseDirectory}.");
    }

    /// <summary>
    /// <c>MemRescue.exe</c> de la misma configuración que estas pruebas (Debug por defecto). Se puede forzar
    /// otro con la variable de entorno <c>MEMRESCUE_EXE</c>.
    /// </summary>
    public static string MemRescueExe
    {
        get
        {
            var forced = Environment.GetEnvironmentVariable("MEMRESCUE_EXE");
            if (!string.IsNullOrEmpty(forced))
            {
                return Path.GetFullPath(forced);
            }
            var config = typeof(RepoPaths).Assembly.GetCustomAttributes<AssemblyMetadataAttribute>()
                .FirstOrDefault(a => a.Key == "Configuration")?.Value ?? "Debug";
            var exe = Path.Combine(Root, "app", "src", "MemRescue", "bin", config, "net10.0-windows", "win-x64", "MemRescue.exe");
            if (!File.Exists(exe))
            {
                throw new FileNotFoundException($"No se encuentra {exe}. Compile antes: dotnet build app\\MemRescue.slnx -c {config}", exe);
            }
            return exe;
        }
    }
}

/// <summary>Huella de un fichero para comprobar que nadie lo ha modificado.</summary>
internal sealed record FileFingerprint(string Path, string Sha256, DateTime LastWriteUtc, long Length)
{
    public static FileFingerprint Of(string path)
    {
        using var fs = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.ReadWrite);
        var hash = Convert.ToHexString(SHA256.HashData(fs));
        var info = new FileInfo(path);
        return new FileFingerprint(path, hash, info.LastWriteTimeUtc, info.Length);
    }
}

/// <summary>
/// Genera una sola vez, en un directorio temporal propio, las imágenes sintéticas deterministas de
/// <c>tests/generators/gen_partition_images.py</c> y las borra al terminar.
/// </summary>
public sealed class TestImages : IDisposable
{
    public string Directory { get; }

    public string GptBasic => Path.Combine(Directory, "gpt_basic.img");

    public string Gpt4Kn => Path.Combine(Directory, "gpt_4kn.img");

    public string MbrBasic => Path.Combine(Directory, "mbr_basic.img");

    /// <summary>gpt_basic.img envuelto en un VHD fijo.</summary>
    public string GptBasicVhd => Path.Combine(Directory, "gpt_basic.vhd");

    /// <summary>VHD dinámico con la entrada de la BAT del bloque 1 fuera del fichero (modo tolerante).</summary>
    public string DamagedVhd => Path.Combine(Directory, "bat_danada.vhd");

    public TestImages()
    {
        Directory = Path.Combine(Path.GetTempPath(), $"memrescue-uitests-{Environment.ProcessId}-{Guid.NewGuid():N}");
        System.IO.Directory.CreateDirectory(Directory);
        var python = Environment.GetEnvironmentVariable("MEMRESCUE_PYTHON") is { Length: > 0 } p ? p : "python";
        var generator = Path.Combine(RepoPaths.Root, "tests", "generators", "gen_partition_images.py");
        var psi = new ProcessStartInfo(python)
        {
            UseShellExecute = false,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            CreateNoWindow = true,
        };
        foreach (var a in new[] { "-I", "-X", "utf8", generator, Directory })
        {
            psi.ArgumentList.Add(a);
        }
        using var proc = Process.Start(psi) ?? throw new InvalidOperationException($"No se pudo ejecutar {python}.");
        var stdout = proc.StandardOutput.ReadToEndAsync();
        var stderr = proc.StandardError.ReadToEndAsync();
        if (!proc.WaitForExit(60_000))
        {
            proc.Kill(entireProcessTree: true);
            throw new TimeoutException("El generador de imágenes no terminó en 60 s.");
        }
        if (proc.ExitCode != 0 || !File.Exists(GptBasic) || !File.Exists(Gpt4Kn) || !File.Exists(MbrBasic))
        {
            throw new InvalidOperationException($"Falló el generador de imágenes (código {proc.ExitCode}):\n{stdout.Result}\n{stderr.Result}");
        }
        File.WriteAllBytes(GptBasicVhd, [.. File.ReadAllBytes(GptBasic), .. FixedVhdFooter(new FileInfo(GptBasic).Length)]);
        File.WriteAllBytes(DamagedVhd, DamagedDynamicVhd());
    }

    /// <summary>Suma de control del VHD: complemento a uno de la suma de los bytes, sin el propio campo.</summary>
    private static void SealVhd(byte[] b, int field)
    {
        uint sum = 0;
        for (var i = 0; i < b.Length; i++)
        {
            if (i < field || i >= field + 4)
            {
                sum += b[i];
            }
        }
        BinaryPrimitives.WriteUInt32BigEndian(b.AsSpan(field), ~sum);
    }

    /// <summary>
    /// VHD dinámico según la especificación pública: 4 bloques de 4 KiB (0, 1 y 2 asignados) con la
    /// entrada de la BAT del bloque 1 fuera del fichero. Cada byte del bloque b vale 0x10 + b.
    /// </summary>
    private static byte[] DamagedDynamicVhd()
    {
        const int bs = 4096;
        var footer = new byte[512];
        "conectix"u8.CopyTo(footer);
        BinaryPrimitives.WriteUInt32BigEndian(footer.AsSpan(8), 2);
        BinaryPrimitives.WriteUInt32BigEndian(footer.AsSpan(12), 0x0001_0000);
        BinaryPrimitives.WriteUInt64BigEndian(footer.AsSpan(16), 512);
        BinaryPrimitives.WriteUInt64BigEndian(footer.AsSpan(40), 4 * bs);
        BinaryPrimitives.WriteUInt64BigEndian(footer.AsSpan(48), 4 * bs);
        BinaryPrimitives.WriteUInt32BigEndian(footer.AsSpan(60), 3);
        SealVhd(footer, 64);
        var header = new byte[1024];
        "cxsparse"u8.CopyTo(header);
        BinaryPrimitives.WriteUInt64BigEndian(header.AsSpan(8), ulong.MaxValue);
        BinaryPrimitives.WriteUInt64BigEndian(header.AsSpan(16), 1536);
        BinaryPrimitives.WriteUInt32BigEndian(header.AsSpan(24), 0x0001_0000);
        BinaryPrimitives.WriteUInt32BigEndian(header.AsSpan(28), 4);
        BinaryPrimitives.WriteUInt32BigEndian(header.AsSpan(32), bs);
        SealVhd(header, 36);
        var bat = Enumerable.Repeat((byte)0xFF, 512).ToArray();
        var body = new List<byte>();
        for (var b = 0; b < 3; b++)
        {
            var sector = (uint)((2048 + body.Count) / 512);
            BinaryPrimitives.WriteUInt32BigEndian(bat.AsSpan(b * 4), b == 1 ? 0x00FF_0000u : sector);
            var bitmap = new byte[512];
            bitmap[0] = 0xFF;
            body.AddRange(bitmap);
            body.AddRange(Enumerable.Repeat((byte)(0x10 + b), bs));
        }
        return [.. footer, .. header, .. bat, .. body, .. footer];
    }

    /// <summary>Pie de un VHD fijo según la especificación pública (512 bytes, big-endian).</summary>
    private static byte[] FixedVhdFooter(long size)
    {
        var f = new byte[512];
        "conectix"u8.CopyTo(f);
        BinaryPrimitives.WriteUInt32BigEndian(f.AsSpan(8), 2);
        BinaryPrimitives.WriteUInt32BigEndian(f.AsSpan(12), 0x0001_0000);
        BinaryPrimitives.WriteUInt64BigEndian(f.AsSpan(16), ulong.MaxValue);
        BinaryPrimitives.WriteUInt64BigEndian(f.AsSpan(40), (ulong)size);
        BinaryPrimitives.WriteUInt64BigEndian(f.AsSpan(48), (ulong)size);
        BinaryPrimitives.WriteUInt32BigEndian(f.AsSpan(60), 2);
        uint sum = 0;
        for (var i = 0; i < 512; i++)
        {
            if (i is < 64 or >= 68)
            {
                sum += f[i];
            }
        }
        BinaryPrimitives.WriteUInt32BigEndian(f.AsSpan(64), ~sum);
        return f;
    }

    public void Dispose()
    {
        try
        {
            System.IO.Directory.Delete(Directory, recursive: true);
        }
        catch (IOException)
        {
            // Si algún proceso aún retiene un fichero, se deja en %TEMP%; no afecta al resultado.
        }
        catch (UnauthorizedAccessException)
        {
        }
    }
}

[CollectionDefinition(Name)]
public sealed class UiCollection : ICollectionFixture<TestImages>
{
    public const string Name = "Interfaz de MemRescue";
}
