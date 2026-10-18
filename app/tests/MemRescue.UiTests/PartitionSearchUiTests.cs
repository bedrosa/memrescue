using System.Windows.Automation;
using Xunit;

namespace MemRescue.UiTests;

/// <summary>
/// Búsqueda exprés de particiones perdidas (issue #15): se lanza solo a petición desde la pestaña de
/// particiones, informa del progreso, lista los volúmenes con indicadores y confianza y abre uno en
/// el visor como un origen independiente.
/// </summary>
[Collection(UiCollection.Name)]
public sealed class PartitionSearchUiTests(TestImages images)
{
    private static List<AutomationElement> Items(AppSession app, string listId) =>
        app.ById(listId).FindAll(TreeScope.Children, new OrCondition(
                new PropertyCondition(AutomationElement.ControlTypeProperty, ControlType.ListItem),
                new PropertyCondition(AutomationElement.ControlTypeProperty, ControlType.DataItem)))
            .Cast<AutomationElement>().ToList();

    [Fact]
    public void Busca_particiones_perdidas_y_abre_un_volumen_encontrado()
    {
        var before = FileFingerprint.Of(images.LostChainThree);
        using (var app = AppSession.Launch(images.LostChainThree))
        {
            app.Invoke("PartitionsToolbarButton");
            app.WaitUntil(() => app.ById("PartitionSchemeText").Current.Name.StartsWith("Esquema:", StringComparison.Ordinal),
                "la tabla leída");
            // La búsqueda no se lanza sola al abrir: solo a petición.
            Assert.Empty(Items(app, "FoundVolumesList"));
            Assert.Contains("solo se hace a petición", app.ById("SearchStatusText").Current.Name, StringComparison.Ordinal);
            Assert.False(app.ById("OpenFoundVolumeButton").Current.IsEnabled);
            app.WaitUntil(() => app.ById("FindPartitionsButton").Current.IsEnabled, "«Buscar particiones perdidas» habilitado");

            app.Invoke("FindPartitionsButton");
            app.WaitUntil(() => Items(app, "FoundVolumesList").Count == 3, "los 3 volúmenes de lost_chain_three");
            Assert.StartsWith("Búsqueda completa: 3 volúmenes (3 fuera de la tabla)", app.ById("SearchStatusText").Current.Name,
                StringComparison.Ordinal);
            var names = Items(app, "FoundVolumesList").Select(i => i.Current.Name).ToList();
            Assert.Equal("Volumen 1: encontrada, NTFS, LBA 2048–4095, 2048 sectores, indicadores Ex P+ C+ Sx U+ A+, " +
                "principal sí, copia sí, confianza válida", names[0]);
            Assert.Contains("FAT32, LBA 4096–71686", names[1], StringComparison.Ordinal);
            Assert.Contains("exFAT, LBA 71687–73734, 2048 sectores, indicadores Ex P+ C+ S+ U+ Ax", names[2], StringComparison.Ordinal);
            Assert.False(app.ById("CancelSearchButton").Current.IsEnabled, "sin búsqueda en curso no se cancela nada");
            Assert.Contains("? no verificable", app.ById("IndicatorsLegendText").Current.Name, StringComparison.Ordinal);

            // Abrir el exFAT (encontrado por encadenamiento, sin alinear) en el visor.
            ((SelectionItemPattern)app.ById("FoundVolume3").GetCurrentPattern(SelectionItemPattern.Pattern)).Select();
            app.WaitUntil(() => app.ById("OpenFoundVolumeButton").Current.IsEnabled, "«Abrir volumen en el visor» habilitado");
            app.Invoke("OpenFoundVolumeButton");
            app.WaitUntil(() => Items(app, "SourcesList").Count == 2, "el volumen como segundo origen");
            app.WaitUntil(() => app.HexText.Contains("EB 76 90 45 58 46 41 54", StringComparison.Ordinal),
                "el sector de arranque exFAT en la LBA 0 del volumen");
            Assert.Equal(0UL, app.ShownLba);
            var texts = Items(app, "SourcesList")[1].FindAll(TreeScope.Descendants,
                    new PropertyCondition(AutomationElement.ControlTypeProperty, ControlType.Text))
                .Cast<AutomationElement>().Select(e => e.Current.Name).ToList();
            Assert.Contains(texts, t => t.StartsWith("lost_chain_three.img [volumen encontrado 3: exFAT LBA 71", StringComparison.Ordinal));
            app.Invoke("LastSectorButton");
            app.WaitForLba(2047);
            Assert.True(app.CloseGracefully(), "MemRescue debe cerrarse limpiamente (código 0).");
        }
        Assert.Equal(before, FileFingerprint.Of(images.LostChainThree));
    }
}
