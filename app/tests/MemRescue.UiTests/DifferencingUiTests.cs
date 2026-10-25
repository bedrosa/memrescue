using System.Windows.Automation;
using Xunit;

namespace MemRescue.UiTests;

/// <summary>
/// Discos diferenciales VHD (incidencia 8): al abrir uno, la interfaz muestra lo que dice de su padre, pide elegir el
/// fichero padre y exige confirmarlo; nunca abre el padre por su cuenta.
/// </summary>
[Collection(UiCollection.Name)]
public sealed class DifferencingUiTests(TestImages images)
{
    private static void Toggle(AppSession app, string id)
    {
        var p = (TogglePattern)app.ById(id).GetCurrentPattern(TogglePattern.Pattern);
        if (p.Current.ToggleState != ToggleState.On)
        {
            p.Toggle();
        }
    }

    private static bool IsEnabled(AppSession app, string id) => app.ById(id).Current.IsEnabled;

    [Fact]
    public void Disco_diferencial_pide_elegir_y_confirmar_el_padre()
    {
        string[] files = [images.ParentVhd, images.ChildVhd, images.OtherParentVhd];
        var before = files.Select(FileFingerprint.Of).ToList();

        // Cancelar: no se abre nada.
        using (var app = AppSession.Launch(images.ChildVhd, waitForViewer: false))
        {
            app.WaitForDialog("Disco diferencial");
            var hints = ((ValuePattern)app.ById("ParentHintsText").GetCurrentPattern(ValuePattern.Pattern)).Current.Value;
            Assert.Contains("parent_name: padre.vhd", hints, StringComparison.Ordinal);
            Assert.Contains("parent_unique_id: b0b0b0b0-", hints, StringComparison.Ordinal);
            Assert.False(IsEnabled(app, "ParentOpenButton"));
            app.Invoke("ParentCancelButton");
            app.WaitUntil(() => app.Status == "Listo", "el estado «Listo»");
            var items = app.ById("SourcesList").FindAll(TreeScope.Children,
                new PropertyCondition(AutomationElement.ControlTypeProperty, ControlType.ListItem));
            Assert.Empty(items);
            Assert.True(app.CloseGracefully(), "MemRescue debe cerrarse limpiamente (código 0).");
        }

        // Padre equivocado: el motor lo rechaza con un error explicado.
        using (var app = AppSession.Launch(images.ChildVhd, waitForViewer: false))
        {
            app.WaitForDialog("Disco diferencial");
            app.SetText("ParentPathBox", images.OtherParentVhd);
            app.Invoke("ParentAddButton");
            app.WaitUntil(() => app.ById("ParentChainStatus").Current.Name.StartsWith("Cadena completa", StringComparison.Ordinal),
                "la cadena completa");
            Assert.False(IsEnabled(app, "ParentOpenButton"), "Abrir exige la confirmación explícita");
            Toggle(app, "ParentConfirmCheck");
            app.WaitUntil(() => IsEnabled(app, "ParentOpenButton"), "el botón Abrir habilitado");
            app.Invoke("ParentOpenButton");
            var error = app.WaitForDialog("No se pudo abrir la imagen");
            Assert.Contains("no corresponde", AppSession.DialogText(error), StringComparison.Ordinal);
            AppSession.ClickDialogButton(error, "2");
            Assert.True(app.CloseGracefully(), "MemRescue debe cerrarse limpiamente (código 0).");
        }

        // Padre correcto y confirmado: se abre y los sectores no escritos vienen del padre.
        using (var app = AppSession.Launch(images.ChildVhd, waitForViewer: false))
        {
            app.WaitForDialog("Disco diferencial");
            app.SetText("ParentPathBox", images.ParentVhd);
            app.Invoke("ParentAddButton");
            Toggle(app, "ParentConfirmCheck");
            app.WaitUntil(() => IsEnabled(app, "ParentOpenButton"), "el botón Abrir habilitado");
            app.Invoke("ParentOpenButton");
            app.WaitUntil(() => app.HexText.Contains("── LBA 0 ·", StringComparison.Ordinal), "el visor con la LBA 0");
            Assert.Contains("40 40 40 40", app.HexText, StringComparison.Ordinal);
            app.SetText("LbaBox", "1");
            app.Invoke("GoLbaButton");
            app.WaitForLba(1);
            Assert.Contains("C1 C1 C1 C1", app.HexText, StringComparison.Ordinal);
            app.SetText("LbaBox", "2");
            app.Invoke("GoLbaButton");
            app.WaitForLba(2);
            Assert.Contains("42 42 42 42", app.HexText, StringComparison.Ordinal);
            Assert.True(app.CloseGracefully(), "MemRescue debe cerrarse limpiamente (código 0).");
        }

        Assert.Equal(before, files.Select(FileFingerprint.Of).ToList());
    }
}
