using System.IO;
using System.Text;
using System.Windows.Automation;
using Xunit;

namespace MemRescue.UiTests;

/// <summary>
/// VMDK con descriptor separado (incidencia 11, ADR-018): al abrir el descriptor se muestran sus
/// extensiones sin abrirlas y cada fichero se confirma por separado; sin todas las confirmaciones
/// no se puede abrir. Ficheros sintéticos en un directorio temporal; se comprueba que no cambian.
/// </summary>
[Collection(UiCollection.Name)]
public sealed class VmdkExtentsUiTests : IDisposable
{
    private readonly string _dir = Directory.CreateTempSubdirectory("mr-ui-vmdk-").FullName;

    public void Dispose()
    {
        try
        {
            Directory.Delete(_dir, recursive: true);
        }
        catch (IOException)
        {
        }
    }

    /// <summary>Descriptor twoGbMaxExtentFlat con dos FLAT de 8 sectores y una ZERO.</summary>
    private (string Descriptor, string[] Files) Build()
    {
        var a = new byte[8 * 512];
        Encoding.ASCII.GetBytes("EXTENSION-UNO").CopyTo(a, 0);
        var b = new byte[8 * 512];
        Array.Fill(b, (byte)0x5A);
        var pa = Path.Combine(_dir, "ui-f001.vmdk");
        var pb = Path.Combine(_dir, "ui-f002.vmdk");
        File.WriteAllBytes(pa, a);
        File.WriteAllBytes(pb, b);
        var desc = Path.Combine(_dir, "ui.vmdk");
        File.WriteAllText(desc,
            "# Disk DescriptorFile\nCID=1\nparentCID=ffffffff\ncreateType=\"twoGbMaxExtentFlat\"\n" +
            "RW 8 FLAT \"ui-f001.vmdk\" 0\nRW 8 FLAT \"ui-f002.vmdk\" 0\nRW 8 ZERO\n", Encoding.ASCII);
        return (desc, [pa, pb]);
    }

    private static AutomationElement In(AutomationElement root, string id) =>
        AppSession.WaitFor(() => root.FindFirst(TreeScope.Descendants,
            new PropertyCondition(AutomationElement.AutomationIdProperty, id)), $"el control «{id}»");

    private static void Toggle(AutomationElement root, string id) =>
        ((TogglePattern)In(root, id).GetCurrentPattern(TogglePattern.Pattern)).Toggle();

    [Fact]
    public void Cada_extension_se_confirma_por_separado_antes_de_abrir()
    {
        var (desc, files) = Build();
        var before = files.Append(desc).Select(FileFingerprint.Of).ToArray();
        using (var app = AppSession.Launch(desc, waitForViewer: false))
        {
            var dlg = app.WaitForDialog("Extensiones VMDK");
            Assert.Contains("twoGbMaxExtentFlat", In(dlg, "ExtentsSummaryText").Current.Name, StringComparison.Ordinal);
            Assert.Equal("ui-f001.vmdk", In(dlg, "ExtentName0").Current.Name);
            Assert.Contains("ceros", In(dlg, "ExtentNote2").Current.Name, StringComparison.Ordinal);
            // Sugerencias del mismo directorio, sin confirmar: no se puede abrir.
            var path0 = ((ValuePattern)In(dlg, "ExtentPath0").GetCurrentPattern(ValuePattern.Pattern)).Current.Value;
            Assert.Equal(files[0], path0, ignoreCase: true);
            var open = In(dlg, "ExtentsOpenButton");
            Assert.False(open.Current.IsEnabled, "Sin confirmar ninguna extensión no se puede abrir.");
            Toggle(dlg, "ExtentConfirm0");
            Assert.False(open.Current.IsEnabled, "Con una sola extensión confirmada no se puede abrir.");
            Toggle(dlg, "ExtentConfirm1");
            app.WaitUntil(() => open.Current.IsEnabled, "el botón de abrir habilitado");
            ((InvokePattern)open.GetCurrentPattern(InvokePattern.Pattern)).Invoke();
            app.WaitUntil(() => app.HexText.Contains("── LBA 0 ·", StringComparison.Ordinal), "el visor con la LBA 0");
            Assert.Contains("EXTENSION-UNO", app.HexText, StringComparison.Ordinal);
            Assert.True(app.CloseGracefully());
        }
        Assert.Equal(before, files.Append(desc).Select(FileFingerprint.Of).ToArray());
    }

    [Fact]
    public void Cancelar_no_abre_ninguna_extension()
    {
        var (desc, files) = Build();
        var before = files.Select(FileFingerprint.Of).ToArray();
        using (var app = AppSession.Launch(desc, waitForViewer: false))
        {
            var dlg = app.WaitForDialog("Extensiones VMDK");
            ((InvokePattern)In(dlg, "ExtentsCancelButton").GetCurrentPattern(InvokePattern.Pattern)).Invoke();
            app.WaitUntil(() => app.Status == "Listo", "el estado «Listo»");
            // Sin origen abierto no hay visor.
            Assert.Null(app.Window.FindFirst(TreeScope.Descendants,
                new PropertyCondition(AutomationElement.AutomationIdProperty, "HexView")));
            Assert.True(app.CloseGracefully());
        }
        Assert.Equal(before, files.Select(FileFingerprint.Of).ToArray());
    }
}
