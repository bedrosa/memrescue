using System.Globalization;
using System.Windows;
using MemRescue.Core.Interop;

namespace MemRescue;

/// <summary>Parámetros del modo de autodiagnóstico.</summary>
internal sealed record SelfTestOptions(string Output, string? Image, uint SectorSize, ulong ReadLba, string? Screenshot);

public partial class App : Application
{
    /// <summary>
    /// Autodiagnóstico: <c>MemRescue.exe --selftest &lt;salida.txt&gt; [imagen] [--sector-size N] [--read-lba N] [--screenshot png]</c>.
    /// Abre la ventana real, carga el motor por la DLL y, si se indica una imagen, la abre, muestra y lee la
    /// LBA pedida, calcula hashes y comprueba su legibilidad; escribe el resultado y se cierra.
    /// </summary>
    internal static SelfTestOptions? SelfTest { get; private set; }

    private static SelfTestOptions? ParseSelfTest(IReadOnlyList<string> args)
    {
        var i = args.ToList().IndexOf("--selftest");
        if (i < 0 || i + 1 >= args.Count)
        {
            return null;
        }
        string? image = null, shot = null;
        uint ss = 512;
        ulong lba = 0;
        for (var k = 0; k < args.Count; k++)
        {
            switch (args[k])
            {
                case "--selftest":
                    k++;
                    break;
                case "--sector-size" when k + 1 < args.Count:
                    ss = uint.Parse(args[++k], CultureInfo.InvariantCulture);
                    break;
                case "--read-lba" when k + 1 < args.Count:
                    lba = ulong.Parse(args[++k], CultureInfo.InvariantCulture);
                    break;
                case "--screenshot" when k + 1 < args.Count:
                    shot = args[++k];
                    break;
                default:
                    image ??= args[k];
                    break;
            }
        }
        return new SelfTestOptions(args[i + 1], image, ss, lba, shot);
    }

    protected override void OnStartup(StartupEventArgs e)
    {
        base.OnStartup(e);
        SelfTest = ParseSelfTest(e.Args);

        try
        {
            Engine.EnsureCompatible();
        }
        catch (Exception ex) when (ex is DllNotFoundException or EntryPointNotFoundException or EngineException)
        {
            var msg = $"No se pudo cargar el motor de MemRescue (memrescue_core.dll):\n\n{ex.Message}";
            if (SelfTest is not null)
            {
                System.IO.File.WriteAllText(SelfTest.Output, $"ERROR\n{msg}\n");
            }
            else
            {
                MessageBox.Show(msg, "MemRescue", MessageBoxButton.OK, MessageBoxImage.Error);
            }
            Shutdown(1);
            return;
        }

        new MainWindow().Show();
    }
}
