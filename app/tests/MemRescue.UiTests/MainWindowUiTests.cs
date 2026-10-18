using System.Windows.Automation;
using Xunit;

namespace MemRescue.UiTests;

/// <summary>
/// Pruebas de extremo a extremo de la ventana principal: se lanza el MemRescue.exe compilado con
/// <c>--open</c> sobre imágenes sintéticas y se maneja como lo haría un usuario (UI Automation).
/// </summary>
[Collection(UiCollection.Name)]
public sealed class MainWindowUiTests(TestImages images)
{
    /// <summary>"EFI PART" en hexadecimal, tal como lo escribe el visor.</summary>
    private const string EfiPartHex = "45 46 49 20 50 41 52 54";

    /// <summary>Línea del volcado que empieza en el desplazamiento <paramref name="offset"/> (formato X12).</summary>
    private static string DumpLine(string hexText, ulong offset)
    {
        var prefix = offset.ToString("X12", System.Globalization.CultureInfo.InvariantCulture) + "  ";
        var line = hexText.Split('\n').FirstOrDefault(l => l.StartsWith(prefix, StringComparison.Ordinal));
        Assert.True(line is not null, $"El visor no contiene la línea del desplazamiento 0x{offset:X}.");
        return line!;
    }

    [Fact]
    public void Abre_la_imagen_indicada_con_open()
    {
        using var app = AppSession.Launch(images.GptBasic);
        Assert.Equal("MemRescue", app.Window.Current.Name);
        Assert.Equal(0UL, app.ShownLba);
        Assert.Contains("sector de 512 B", app.SectorInfo, StringComparison.Ordinal);
        var sources = app.ById("SourcesList");
        var items = sources.FindAll(TreeScope.Children, new PropertyCondition(AutomationElement.ControlTypeProperty, ControlType.ListItem))
            .Cast<AutomationElement>().ToList();
        var item = Assert.Single(items);
        var texts = item.FindAll(TreeScope.Descendants, new PropertyCondition(AutomationElement.ControlTypeProperty, ControlType.Text))
            .Cast<AutomationElement>().Select(e => e.Current.Name).ToList();
        Assert.Contains("gpt_basic.img", texts);
    }

    [Fact]
    public void Navegacion_con_caja_LBA_y_botones()
    {
        using var app = AppSession.Launch(images.GptBasic);

        // «1» + «Ir»: cabecera GPT primaria
        app.SetText("LbaBox", "1");
        app.Invoke("GoLbaButton");
        app.WaitForLba(1);
        var line = DumpLine(app.HexText, 0x200);
        Assert.Contains(EfiPartHex, line, StringComparison.Ordinal);
        Assert.Contains("EFI PART", line, StringComparison.Ordinal);

        // ▶ → LBA 2
        app.Invoke("NextSectorButton");
        app.WaitForLba(2);
        Assert.StartsWith("LBA 2 de ", app.SectorInfo, StringComparison.Ordinal);

        // |◀ → LBA 0: MBR protector con la firma 55 AA en 0x1FE
        app.Invoke("FirstSectorButton");
        app.WaitForLba(0);
        Assert.Matches(@"^0000000001F0  (?:[0-9A-F]{2} ){8} (?:[0-9A-F]{2} ){6}55 AA ", DumpLine(app.HexText, 0x1F0));

        // ◀ en la LBA 0 no se sale del rango
        app.Invoke("PrevSectorButton");
        app.WaitForLba(0);

        // ▶| → última LBA (32767): cabecera GPT de respaldo
        app.Invoke("LastSectorButton");
        app.WaitForLba(32767);
        var last = DumpLine(app.HexText, 32767UL * 512);
        Assert.Contains(EfiPartHex, last, StringComparison.Ordinal);
        Assert.Contains("EFI PART", last, StringComparison.Ordinal);
        Assert.Matches(@"^LBA 32\D?767 / 32\D?767$", app.Position);
    }

    [Fact]
    public void LBA_fuera_de_rango_no_cambia_la_vista()
    {
        using var app = AppSession.Launch(images.GptBasic);
        app.SetText("LbaBox", "1");
        app.Invoke("GoLbaButton");
        app.WaitForLba(1);
        var hexBefore = app.HexText;
        var infoBefore = app.SectorInfo;

        app.SetText("LbaBox", "99999999");
        app.Invoke("GoLbaButton");
        app.WaitUntil(() => app.Status.StartsWith("LBA no válida", StringComparison.Ordinal), "el aviso «LBA no válida»");
        Assert.Matches(@"^LBA no válida \(0 – 32\D?767\)$", app.Status);

        // Texto no numérico: mismo tratamiento
        app.SetText("LbaBox", "xyz");
        app.Invoke("GoLbaButton");
        app.WaitUntil(() => app.Status.StartsWith("LBA no válida", StringComparison.Ordinal), "el aviso «LBA no válida»");

        // Se da tiempo a que una posible lectura (incorrecta) llegara al visor antes de comprobar que no cambió.
        app.Invoke("GoLbaButton");
        app.WaitUntil(() => app.Status.StartsWith("LBA no válida", StringComparison.Ordinal), "el aviso «LBA no válida»");
        Assert.Equal(hexBefore, app.HexText);
        Assert.Equal(infoBefore, app.SectorInfo);
        Assert.Equal(1UL, app.ShownLba);

        // Una LBA válida posterior vuelve a funcionar y restablece el estado
        app.SetText("LbaBox", "0x2");
        app.Invoke("GoLbaButton");
        app.WaitForLba(2);
        Assert.Equal("Listo", app.Status);
    }

    [Fact]
    public void Menus_pendientes_deshabilitados_e_ir_a_sector_habilitado()
    {
        using var app = AppSession.Launch(images.GptBasic);

        var partitions = app.OpenMenuItem("DiskMenu", "PartitionsMenuItem");
        Assert.Equal("Particiones…", partitions.Current.Name);
        Assert.True(partitions.Current.IsEnabled, "«Particiones…» está habilitado desde M2 (lista de particiones).");
        app.CloseMenu("DiskMenu");

        var goTo = app.OpenMenuItem("EditorMenu", "GoToSectorMenuItem");
        Assert.Equal("Ir a sector…", goTo.Current.Name);
        app.WaitUntil(() => goTo.Current.IsEnabled, "«Ir a sector…» habilitado con una imagen abierta");
        var cluster = app.ById("GoToClusterMenuItem");
        Assert.False(cluster.Current.IsEnabled, "«Ir a cluster / registro…» debe estar deshabilitado (pendiente, M3).");
        app.CloseMenu("EditorMenu");
        Assert.True(app.CloseGracefully(), "MemRescue debe cerrarse limpiamente tras usar los menús.");
    }

    [Fact]
    public void Sector_de_4096_bytes()
    {
        using var app = AppSession.Launch(images.Gpt4Kn, sectorSize: 4096);
        Assert.Contains("sector de 4096 B", app.SectorInfo, StringComparison.Ordinal);
        app.Invoke("NextSectorButton");
        app.WaitForLba(1);
        Assert.Contains("(0x1000)", app.SectorInfo, StringComparison.Ordinal);
        var line = DumpLine(app.HexText, 0x1000);
        Assert.Contains(EfiPartHex, line, StringComparison.Ordinal);
        Assert.Contains("EFI PART", line, StringComparison.Ordinal);
        // Un sector completo de 4096 B son 256 líneas de volcado
        Assert.Equal(256, app.HexText.Split('\n').Count(l => l.Length > 12 && Uri.IsHexDigit(l[0]) && l[12] == ' '));
    }

    [Fact]
    public void Particion_MBR_en_LBA_2048()
    {
        using var app = AppSession.Launch(images.MbrBasic);
        app.SetText("LbaBox", "2048");
        app.Invoke("GoLbaButton");
        app.WaitForLba(2048);
        // Marcador del generador: "MEMRESCUE-TEST|mbr_basic|P1|LBA=2048|" (las 16 primeras letras en la primera línea)
        Assert.EndsWith("MEMRESCUE-TEST|m", DumpLine(app.HexText, 2048UL * 512), StringComparison.Ordinal);
        Assert.EndsWith("br_basic|P1|LBA=", DumpLine(app.HexText, 2048UL * 512 + 16), StringComparison.Ordinal);
    }

    [Fact]
    public void Contenedor_danado_pide_confirmacion_para_el_modo_tolerante()
    {
        var before = FileFingerprint.Of(images.DamagedVhd);

        // «No»: la imagen no se abre.
        using (var app = AppSession.Launch(images.DamagedVhd, waitForViewer: false))
        {
            var dialog = app.WaitForDialog("Modo tolerante");
            var text = AppSession.DialogText(dialog);
            Assert.Contains("MODO TOLERANTE", text, StringComparison.Ordinal);
            Assert.Contains("1 bloque descartado", text, StringComparison.Ordinal);
            AppSession.ClickDialogButton(dialog, "7");
            app.WaitUntil(() => app.Status == "Listo", "el estado «Listo»");
            var items = app.ById("SourcesList").FindAll(TreeScope.Children,
                new PropertyCondition(AutomationElement.ControlTypeProperty, ControlType.ListItem));
            Assert.Empty(items);
            Assert.True(app.CloseGracefully(), "MemRescue debe cerrarse limpiamente (código 0).");
        }

        // «Sí»: se abre con un aviso visible y los sectores descartados son ilegibles.
        using (var app = AppSession.Launch(images.DamagedVhd, waitForViewer: false))
        {
            AppSession.ClickDialogButton(app.WaitForDialog("Modo tolerante"), "6");
            app.WaitUntil(() => app.HexText.Contains("── LBA 0 ·", StringComparison.Ordinal), "el visor con la LBA 0");
            var banner = app.ById("TolerantBannerText").Current.Name;
            Assert.Contains("MODO TOLERANTE", banner, StringComparison.Ordinal);
            Assert.Contains("1 bloque descartado", banner, StringComparison.Ordinal);
            app.SetText("LbaBox", "8");
            app.Invoke("GoLbaButton");
            app.WaitForLba(8);
            app.WaitUntil(() => app.ById("UntrustedBannerText").Current.Name.Length > 0, "el aviso de sectores no fiables");
            Assert.Contains("BAD!", DumpLine(app.HexText, 8 * 512), StringComparison.Ordinal);
            Assert.True(app.CloseGracefully(), "MemRescue debe cerrarse limpiamente (código 0).");
        }

        Assert.Equal(before, FileFingerprint.Of(images.DamagedVhd));
    }

    [Fact]
    public void Las_imagenes_no_se_modifican()
    {
        string[] paths = [images.GptBasic, images.Gpt4Kn, images.MbrBasic];
        var before = paths.Select(FileFingerprint.Of).ToList();

        using (var app = AppSession.Launch(images.GptBasic))
        {
            foreach (var id in new[] { "NextSectorButton", "LastSectorButton", "PrevSectorButton", "FirstSectorButton" })
            {
                app.Invoke(id);
            }
            app.WaitForLba(0);
            Assert.True(app.CloseGracefully(), "MemRescue debe cerrarse limpiamente (código 0).");
        }
        using (var app = AppSession.Launch(images.Gpt4Kn, sectorSize: 4096))
        {
            app.Invoke("LastSectorButton");
            app.WaitForLba(2047);
            Assert.True(app.CloseGracefully(), "MemRescue debe cerrarse limpiamente (código 0).");
        }
        using (var app = AppSession.Launch(images.MbrBasic))
        {
            app.SetText("LbaBox", "2048");
            app.Invoke("GoLbaButton");
            app.WaitForLba(2048);
            Assert.True(app.CloseGracefully(), "MemRescue debe cerrarse limpiamente (código 0).");
        }

        var after = paths.Select(FileFingerprint.Of).ToList();
        Assert.Equal(before, after);
    }

    [Fact]
    public void Contenedor_VHD_se_interpreta_y_cada_sector_muestra_su_estado()
    {
        var before = FileFingerprint.Of(images.GptBasicVhd);
        using (var app = AppSession.Launch(images.GptBasicVhd))
        {
            Assert.Contains("leído", app.SectorInfo, StringComparison.Ordinal);
            Assert.Contains("── LBA 0 · desplazamiento 0 (0x0) · leído ──", app.HexText, StringComparison.Ordinal);
            app.SetText("LbaBox", "1");
            app.Invoke("GoLbaButton");
            app.WaitForLba(1);
            Assert.Contains(EfiPartHex, DumpLine(app.HexText, 0x200), StringComparison.Ordinal);
            // El pie del VHD no forma parte del disco: la última LBA es la del disco contenido.
            app.Invoke("LastSectorButton");
            app.WaitForLba(32767);
            Assert.Contains(EfiPartHex, DumpLine(app.HexText, 32767UL * 512), StringComparison.Ordinal);
            // Sin sectores no fiables, el aviso no se muestra.
            Assert.Null(app.Window.FindFirst(TreeScope.Descendants,
                new PropertyCondition(AutomationElement.AutomationIdProperty, "UntrustedBannerText")));
            Assert.True(app.CloseGracefully(), "MemRescue debe cerrarse limpiamente (código 0).");
        }
        Assert.Equal(before, FileFingerprint.Of(images.GptBasicVhd));
    }

    [Fact]
    public void Barra_de_posicion_recorre_la_imagen()
    {
        using var app = AppSession.Launch(images.GptBasic);
        var range = (RangeValuePattern)app.ById("LbaScrollBar").GetCurrentPattern(RangeValuePattern.Pattern);
        Assert.Equal(32767d, range.Current.Maximum);
        range.SetValue(32767);
        app.WaitForLba(32767);
        Assert.Contains(EfiPartHex, DumpLine(app.HexText, 32767UL * 512), StringComparison.Ordinal);
        range.SetValue(1);
        app.WaitForLba(1);
        app.Invoke("NextSectorButton");
        app.WaitForLba(2);
        app.WaitUntil(() => range.Current.Value == 2d, "la barra de posición sincronizada con la LBA 2");
    }

    [Fact]
    public void Comprobacion_con_politica_de_errores()
    {
        using var app = AppSession.Launch(images.GptBasic);
        ((SelectionItemPattern)app.ById("ChecksTab").GetCurrentPattern(SelectionItemPattern.Pattern)).Select();
        string Results() => ((ValuePattern)app.ById("VerifyResults").GetCurrentPattern(ValuePattern.Pattern)).Current.Value;

        app.SetText("SkipCodesBox", "1117, 23");
        app.Invoke("VerifyStartButton");
        app.WaitUntil(() => Results().Contains("Política de errores: sin reintentos para los códigos 23, 1117", StringComparison.Ordinal),
            "el informe con la lista de códigos", TimeSpan.FromSeconds(60));
        Assert.Contains("errores omitidos (ilegibles sin reintentar)  0", Results(), StringComparison.Ordinal);

        var ignore = (TogglePattern)app.ById("IgnoreErrorsBox").GetCurrentPattern(TogglePattern.Pattern);
        ignore.Toggle();
        app.WaitUntil(() => !app.ById("SkipCodesBox").Current.IsEnabled, "la lista de códigos deshabilitada");
        app.WaitUntil(() => app.ById("VerifyStartButton").Current.IsEnabled, "el botón Comprobar habilitado");
        app.Invoke("VerifyStartButton");
        app.WaitUntil(() => Results().Contains("Política de errores: ignorar todos los errores (sin reintentos)", StringComparison.Ordinal),
            "el informe con «ignorar todo»", TimeSpan.FromSeconds(60));
        Assert.Contains("ilegibles          0", Results(), StringComparison.Ordinal);
        ignore.Toggle();
        app.WaitUntil(() => app.ById("SkipCodesBox").Current.IsEnabled, "la lista de códigos habilitada de nuevo");
    }
}
