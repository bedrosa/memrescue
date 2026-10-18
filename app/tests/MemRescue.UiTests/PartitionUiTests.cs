using System.Globalization;
using System.Windows.Automation;
using Xunit;

namespace MemRescue.UiTests;

/// <summary>
/// Lista de particiones del origen abierto (issue #12): esquema, confianza y anomalías, y apertura de
/// una partición en el visor como un origen independiente.
/// </summary>
[Collection(UiCollection.Name)]
public sealed class PartitionUiTests(TestImages images)
{
    private static void SelectTab(AppSession app, string id) =>
        ((SelectionItemPattern)app.ById(id).GetCurrentPattern(SelectionItemPattern.Pattern)).Select();

    private static List<AutomationElement> Items(AppSession app, string listId) =>
        app.ById(listId).FindAll(TreeScope.Children, new OrCondition(
                new PropertyCondition(AutomationElement.ControlTypeProperty, ControlType.ListItem),
                new PropertyCondition(AutomationElement.ControlTypeProperty, ControlType.DataItem)))
            .Cast<AutomationElement>().ToList();

    private static string DumpLine(string hexText, ulong offset)
    {
        var prefix = offset.ToString("X12", CultureInfo.InvariantCulture) + "  ";
        return hexText.Split('\n').FirstOrDefault(l => l.StartsWith(prefix, StringComparison.Ordinal)) ?? "";
    }

    [Fact]
    public void Lista_las_particiones_y_abre_una_en_el_visor()
    {
        var before = FileFingerprint.Of(images.MbrBasic);
        using (var app = AppSession.Launch(images.MbrBasic))
        {
            // El menú y el botón de la barra llevan a la pestaña de particiones.
            var menu = app.OpenMenuItem("DiskMenu", "PartitionsMenuItem");
            Assert.True(menu.Current.IsEnabled, "«Particiones…» debe estar habilitado (M2).");
            ((InvokePattern)menu.GetCurrentPattern(InvokePattern.Pattern)).Invoke();
            app.WaitUntil(() => ((SelectionItemPattern)app.ById("PartitionsTab").GetCurrentPattern(SelectionItemPattern.Pattern)).Current.IsSelected,
                "la pestaña «Particiones» seleccionada");

            app.WaitUntil(() => Items(app, "PartitionsList").Count == 4, "las 4 particiones de mbr_basic");
            Assert.StartsWith("Esquema: MBR · 4 particiones · confianza del conjunto: válida", app.ById("PartitionSchemeText").Current.Name,
                StringComparison.Ordinal);
            var names = Items(app, "PartitionsList").Select(i => i.Current.Name).ToList();
            Assert.Equal("Partición 1: primaria 1, LBA 2048–10239, 8192 sectores, 0x07 NTFS / exFAT / HPFS, activa, confianza válida", names[0]);
            Assert.Contains("lógica (EBR 14336), LBA 16384–20479", names[2], StringComparison.Ordinal);
            Assert.Contains("0x83 Linux", names[2], StringComparison.Ordinal);
            Assert.Equal(["(ninguna)"], Items(app, "PartitionAnomaliesList").Select(i => i.Current.Name));
            Assert.False(app.ById("OpenPartitionButton").Current.IsEnabled, "sin selección no se puede abrir");

            // Seleccionar la lógica L1 y abrirla en el visor.
            ((SelectionItemPattern)app.ById("Partition3").GetCurrentPattern(SelectionItemPattern.Pattern)).Select();
            app.WaitUntil(() => app.ById("OpenPartitionButton").Current.IsEnabled, "«Abrir en el visor» habilitado");
            app.Invoke("OpenPartitionButton");
            app.WaitUntil(() => Items(app, "SourcesList").Count == 2, "la partición como segundo origen");
            // El visor muestra la LBA 0 de la partición: el marcador del generador en la LBA 16384 del disco.
            app.WaitUntil(() => DumpLine(app.HexText, 0).EndsWith("MEMRESCUE-TEST|m", StringComparison.Ordinal)
                && DumpLine(app.HexText, 16).EndsWith("br_basic|L1|LBA=", StringComparison.Ordinal),
                "el primer sector de la partición en el visor");
            Assert.Equal(0UL, app.ShownLba);
            var texts = Items(app, "SourcesList")[1].FindAll(TreeScope.Descendants,
                    new PropertyCondition(AutomationElement.ControlTypeProperty, ControlType.Text))
                .Cast<AutomationElement>().Select(e => e.Current.Name).ToList();
            Assert.Contains(texts, t => t.StartsWith("mbr_basic.img [partición 3: LBA 16", StringComparison.Ordinal));
            // La última LBA de la partición es la 4095 (4096 sectores).
            app.Invoke("LastSectorButton");
            app.WaitForLba(4095);
            Assert.True(app.CloseGracefully(), "MemRescue debe cerrarse limpiamente (código 0).");
        }
        Assert.Equal(before, FileFingerprint.Of(images.MbrBasic));
    }

    [Fact]
    public void Lista_las_particiones_GPT_y_abre_una()
    {
        using var app = AppSession.Launch(images.GptBasic);
        SelectTab(app, "PartitionsTab");
        app.WaitUntil(() => Items(app, "PartitionsList").Count == 3, "las 3 particiones GPT de gpt_basic");
        Assert.Equal("Esquema: GPT · 3 particiones · confianza del conjunto: válida · copia GPT: primaria",
            app.ById("PartitionSchemeText").Current.Name);
        var names = Items(app, "PartitionsList").Select(i => i.Current.Name).ToList();
        Assert.Equal("Partición 1: GPT 1, LBA 2048–10239, 8192 sectores, Datos básicos de Microsoft, «MR-DATA1», confianza válida", names[0]);
        Assert.Contains("Sistema de archivos Linux, «MR-LINUX»", names[1], StringComparison.Ordinal);
        Assert.Contains("LBA 22528–24575, 2048 sectores, Sistema EFI, «MR-ESP»", names[2], StringComparison.Ordinal);
        Assert.Equal(["(ninguna)"], Items(app, "PartitionAnomaliesList").Select(i => i.Current.Name));

        ((SelectionItemPattern)app.ById("Partition3").GetCurrentPattern(SelectionItemPattern.Pattern)).Select();
        app.WaitUntil(() => app.ById("OpenPartitionButton").Current.IsEnabled, "«Abrir en el visor» habilitado");
        app.Invoke("OpenPartitionButton");
        app.WaitUntil(() => Items(app, "SourcesList").Count == 2, "la partición GPT como segundo origen");
        app.WaitUntil(() => DumpLine(app.HexText, 0).EndsWith("MEMRESCUE-TEST|g", StringComparison.Ordinal)
            && DumpLine(app.HexText, 16).EndsWith("pt_basic|E1|LBA=", StringComparison.Ordinal),
            "el primer sector de la partición EFI en el visor");
        app.Invoke("LastSectorButton");
        app.WaitForLba(2047);
        Assert.True(app.CloseGracefully(), "MemRescue debe cerrarse limpiamente (código 0).");
    }

    [Fact]
    public void Muestra_la_confianza_y_las_anomalias_de_la_tabla()
    {
        using var app = AppSession.Launch(images.MbrEbrLoop);
        app.Invoke("PartitionsToolbarButton");
        app.WaitUntil(() => Items(app, "PartitionsList").Count == 3, "las 3 particiones de mbr_ebr_loop");
        Assert.Contains("confianza del conjunto: SOSPECHOSA", app.ById("PartitionSchemeText").Current.Name, StringComparison.Ordinal);
        var anomalies = Items(app, "PartitionAnomaliesList").Select(i => i.Current.Name).ToList();
        var loop = Assert.Single(anomalies);
        Assert.StartsWith("Tabla: [sospechosa] ebr_loop: el enlace del EBR de la LBA 3072 vuelve al EBR de la LBA 2560", loop,
            StringComparison.Ordinal);
        SelectTab(app, "SectorsTab");
        Assert.True(app.CloseGracefully(), "MemRescue debe cerrarse limpiamente (código 0).");
    }
}
