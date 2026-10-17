using System.Globalization;
using System.IO;
using System.Windows.Automation;
using Xunit;

namespace MemRescue.UiTests;

/// <summary>
/// Colores por estado de sector y patrones de relleno configurables (IO-08, HX-01) sobre un origen con
/// sectores no fiables: VHD dinámico con un bloque descartado en modo tolerante (LBA 8–15 ilegibles,
/// 24–31 dispersos). Las preferencias se guardan en un directorio temporal, nunca en el perfil real.
/// </summary>
[Collection(UiCollection.Name)]
public sealed class FillPatternUiTests(TestImages images)
{
    /// <summary>Línea del volcado que empieza en el desplazamiento <paramref name="offset"/> (formato X12).</summary>
    private static string DumpLine(string hexText, ulong offset)
    {
        var prefix = offset.ToString("X12", CultureInfo.InvariantCulture) + "  ";
        var line = hexText.Split('\n').FirstOrDefault(l => l.StartsWith(prefix, StringComparison.Ordinal));
        Assert.True(line is not null, $"El visor no contiene la línea del desplazamiento 0x{offset:X}.");
        return line!;
    }

    private static AutomationElement Find(AutomationElement root, string automationId) =>
        AppSession.WaitFor(() => root.FindFirst(TreeScope.Descendants,
            new PropertyCondition(AutomationElement.AutomationIdProperty, automationId)), $"el control «{automationId}»");

    private static void SetValue(AutomationElement root, string automationId, string text) =>
        ((ValuePattern)Find(root, automationId).GetCurrentPattern(ValuePattern.Pattern)).SetValue(text);

    private static string Value(AutomationElement root, string automationId) =>
        ((ValuePattern)Find(root, automationId).GetCurrentPattern(ValuePattern.Pattern)).Current.Value;

    private static void Press(AutomationElement root, string automationId) =>
        ((InvokePattern)Find(root, automationId).GetCurrentPattern(InvokePattern.Pattern)).Invoke();

    /// <summary>Lanza MemRescue con el VHD dañado y acepta el modo tolerante.</summary>
    private AppSession LaunchDamaged(string? settingsDir = null)
    {
        var app = AppSession.Launch(images.DamagedVhd, waitForViewer: false, settingsDir: settingsDir);
        AppSession.ClickDialogButton(app.WaitForDialog("Modo tolerante"), "6");
        app.WaitUntil(() => app.HexText.Contains("── LBA 0 ·", StringComparison.Ordinal), "el visor con la LBA 0");
        return app;
    }

    private static void GoTo(AppSession app, ulong lba)
    {
        app.SetText("LbaBox", lba.ToString(CultureInfo.InvariantCulture));
        app.Invoke("GoLbaButton");
        app.WaitForLba(lba);
    }

    private static void SelectPageSize(AppSession app, string size)
    {
        var box = app.ById("PageSizeBox");
        ((ExpandCollapsePattern)box.GetCurrentPattern(ExpandCollapsePattern.Pattern)).Expand();
        var item = AppSession.WaitFor(() => box.FindFirst(TreeScope.Descendants, new AndCondition(
            new PropertyCondition(AutomationElement.ControlTypeProperty, ControlType.ListItem),
            new PropertyCondition(AutomationElement.NameProperty, size))), $"el elemento «{size}»");
        ((SelectionItemPattern)item.GetCurrentPattern(SelectionItemPattern.Pattern)).Select();
        ((ExpandCollapsePattern)box.GetCurrentPattern(ExpandCollapsePattern.Pattern)).Collapse();
    }

    [Fact]
    public void Cada_sector_muestra_su_estado_con_color_y_texto()
    {
        var before = FileFingerprint.Of(images.DamagedVhd);
        using (var app = LaunchDamaged())
        {
            // Leyenda: un elemento por estado (con texto) y los patrones en uso.
            Assert.Equal("LEÍDO", app.ById("LegendRead").Current.Name);
            Assert.Equal("RECUPERADO", app.ById("LegendRecovered").Current.Name);
            Assert.Equal("ILEGIBLE · RELLENO", app.ById("LegendUnreadable").Current.Name);
            Assert.Equal("SALTADO · RELLENO", app.ById("LegendSkipped").Current.Name);
            Assert.Equal("DISPERSO · CEROS", app.ById("LegendSparse").Current.Name);
            var fills = app.ById("FillPatternsText").Current.Name;
            Assert.Contains("«BAD!» (42 41 44 21)", fills, StringComparison.Ordinal);
            Assert.Contains("«SKIP» (53 4B 49 50)", fills, StringComparison.Ordinal);
            Assert.Contains("nunca son datos", fills, StringComparison.Ordinal);
            Assert.Equal("LBA 0: LEÍDO", app.SectorStateChip(0));

            // Página de 8 sectores que cruza datos (6–7), ilegibles (8–13).
            SelectPageSize(app, "8");
            app.SetText("LbaBox", "6");
            app.Invoke("GoLbaButton");
            app.WaitUntil(() => app.SectorStateChip(13).Length > 0 && app.SectorInfo.StartsWith("LBA 6 – 13 de ", StringComparison.Ordinal),
                "la página de 8 sectores desde la LBA 6");
            Assert.Equal("LBA 6: LEÍDO", app.SectorStateChip(6));
            Assert.Equal("LBA 7: LEÍDO", app.SectorStateChip(7));
            for (ulong l = 8; l <= 13; l++)
            {
                Assert.Equal($"LBA {l}: ILEGIBLE · RELLENO", app.SectorStateChip(l));
            }
            var hex = app.HexText;
            Assert.Equal(6, hex.Split('\n').Count(l => l.StartsWith("!! SECTOR NO FIABLE", StringComparison.Ordinal)));
            Assert.Contains("PATRÓN DE RELLENO «BAD!» (42 41 44 21) generado por MemRescue, NO datos del origen", hex, StringComparison.Ordinal);
            Assert.Contains("ILEGIBLE: bytes de relleno, no datos", hex, StringComparison.Ordinal);
            Assert.Contains("BAD!", DumpLine(hex, 8 * 512), StringComparison.Ordinal);
            var banner = app.ById("UntrustedBannerText").Current.Name;
            Assert.Contains("6 de 8 sectores", banner, StringComparison.Ordinal);
            Assert.Contains("no datos del origen", banner, StringComparison.Ordinal);

            // Dispersos: estado propio, ceros legítimos, sin aviso de relleno.
            app.SetText("LbaBox", "24");
            app.Invoke("GoLbaButton");
            app.WaitUntil(() => app.SectorStateChip(31).Length > 0 && app.SectorInfo.StartsWith("LBA 24 – 31 de ", StringComparison.Ordinal),
                "la página de 8 sectores desde la LBA 24");
            Assert.Equal("LBA 24: DISPERSO · CEROS", app.SectorStateChip(24));
            Assert.Equal("LBA 31: DISPERSO · CEROS", app.SectorStateChip(31));
            Assert.DoesNotContain("NO FIABLE", app.HexText, StringComparison.Ordinal);
            Assert.Contains("disperso (ceros del formato)", app.HexText, StringComparison.Ordinal);
            Assert.True(app.CloseGracefully(), "MemRescue debe cerrarse limpiamente (código 0).");
        }
        Assert.Equal(before, FileFingerprint.Of(images.DamagedVhd));
    }

    [Fact]
    public void Patrones_de_relleno_se_validan_y_persisten()
    {
        var before = FileFingerprint.Of(images.DamagedVhd);
        var settingsDir = AppSession.NewSettingsDirectory();
        var settingsFile = Path.Combine(settingsDir, "settings.json");
        try
        {
            using (var app = LaunchDamaged(settingsDir))
            {
                GoTo(app, 8);
                Assert.Contains("BAD!", DumpLine(app.HexText, 8 * 512), StringComparison.Ordinal);

                var item = app.OpenMenuItem("ToolsMenu", "FillPatternsMenuItem");
                ((InvokePattern)item.GetCurrentPattern(InvokePattern.Pattern)).Invoke();
                var dialog = app.WaitForDialog("Patrones de relleno");
                Assert.Equal("BAD!", Value(dialog, "BadFillBox"));
                Assert.Equal("SKIP", Value(dialog, "SkipFillBox"));
                Assert.Contains("NUNCA son datos", AppSession.DialogText(dialog), StringComparison.Ordinal);
                var save = Find(dialog, "FillPatternsSaveButton");
                string Error() => Find(dialog, "FillPatternsError").Current.Name;

                // Ceros: indistinguibles de dispersos.
                SetValue(dialog, "BadFillBox", "00000000");
                app.WaitUntil(() => Error().Contains("00 00 00 00", StringComparison.Ordinal), "el error del patrón a cero");
                Assert.False(save.Current.IsEnabled, "No se debe poder guardar un patrón a cero.");
                // Bytes iguales y texto no válido.
                SetValue(dialog, "BadFillBox", "FFFFFFFF");
                app.WaitUntil(() => Error().Contains("cuatro bytes iguales", StringComparison.Ordinal), "el error del patrón uniforme");
                SetValue(dialog, "BadFillBox", "XYZ");
                app.WaitUntil(() => Error().Contains("Patrón no válido", StringComparison.Ordinal), "el error de formato");
                // Mismo patrón para ambos estados.
                SetValue(dialog, "BadFillBox", "SKIP");
                app.WaitUntil(() => Error().Contains("distintos", StringComparison.Ordinal), "el error de patrones iguales");
                Assert.False(save.Current.IsEnabled);
                // Restablecer.
                Press(dialog, "FillPatternsResetButton");
                app.WaitUntil(() => Value(dialog, "BadFillBox") == "BAD!" && Error().Length == 0, "los valores por defecto");
                // Patrones propios válidos.
                SetValue(dialog, "BadFillBox", "DEADBEEF");
                SetValue(dialog, "SkipFillBox", "sk1p");
                app.WaitUntil(() => save.Current.IsEnabled, "el botón Guardar habilitado");
                Assert.Equal("DE AD BE EF", Find(dialog, "BadFillPreview").Current.Name);
                Press(dialog, "FillPatternsSaveButton");

                // El visor se recompone con el patrón nuevo, sin dejar de marcarlo como relleno.
                app.WaitUntil(() => DumpLine(app.HexText, 8 * 512).Contains("DE AD BE EF DE AD BE EF", StringComparison.Ordinal),
                    "el relleno configurado en la LBA 8");
                Assert.Contains("PATRÓN DE RELLENO DE AD BE EF generado por MemRescue, NO datos del origen", app.HexText, StringComparison.Ordinal);
                Assert.Equal("LBA 8: ILEGIBLE · RELLENO", app.SectorStateChip(8));
                Assert.Contains("«DEADBEEF»", app.ById("UntrustedBannerText").Current.Name, StringComparison.Ordinal);
                Assert.Contains("ilegibles DE AD BE EF, saltados «sk1p» (73 6B 31 70)", app.ById("FillPatternsText").Current.Name, StringComparison.Ordinal);
                Assert.True(File.Exists(settingsFile), "Las preferencias deben guardarse en el directorio indicado.");
                Assert.Contains("DEADBEEF", File.ReadAllText(settingsFile), StringComparison.Ordinal);
                Assert.True(app.CloseGracefully(), "MemRescue debe cerrarse limpiamente (código 0).");
            }

            // Persistencia: otra ejecución con las mismas preferencias usa el patrón guardado.
            using (var app = LaunchDamaged(settingsDir))
            {
                GoTo(app, 8);
                Assert.Contains("DE AD BE EF DE AD BE EF", DumpLine(app.HexText, 8 * 512), StringComparison.Ordinal);
                Assert.True(app.CloseGracefully(), "MemRescue debe cerrarse limpiamente (código 0).");
            }

            // Preferencias dañadas: valores por defecto, sin fallar ni sobrescribir el fichero.
            File.WriteAllText(settingsFile, "{ dañado");
            using (var app = LaunchDamaged(settingsDir))
            {
                GoTo(app, 8);
                Assert.Contains("BAD!", DumpLine(app.HexText, 8 * 512), StringComparison.Ordinal);
                Assert.True(app.CloseGracefully(), "MemRescue debe cerrarse limpiamente (código 0).");
            }
            Assert.Equal("{ dañado", File.ReadAllText(settingsFile));
        }
        finally
        {
            Directory.Delete(settingsDir, recursive: true);
        }
        // Nada se escribe junto a la imagen y la imagen no cambia.
        Assert.False(File.Exists(Path.Combine(Path.GetDirectoryName(images.DamagedVhd)!, "settings.json")));
        Assert.Equal(before, FileFingerprint.Of(images.DamagedVhd));
    }
}
