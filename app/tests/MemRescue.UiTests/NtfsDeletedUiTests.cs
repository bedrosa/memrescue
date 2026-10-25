using System.IO;
using System.Security.Cryptography;
using System.Windows.Automation;
using Xunit;

namespace MemRescue.UiTests;

/// <summary>
/// Borrados en la pestaña «Sistema de archivos» (#23): sobre <c>ntfs_deleted.img</c> (corpus NTFS) se abre el
/// sistema de archivos, se activa «Mostrar borrados», se llega al directorio borrado <c>/docs/cartas</c> por el
/// árbol, se recupera <c>carta_03.txt</c> en un directorio temporal (SHA-256 del manifiesto) y se comprueba
/// que al desactivarlo los borrados desaparecen de la lista.
/// </summary>
[Collection(UiCollection.Name)]
[Trait("Category", "NtfsCorpus")]
public sealed class NtfsDeletedUiTests
{
    private static string CorpusImage(string name)
    {
        var dir = Environment.GetEnvironmentVariable("MR_NTFS_CORPUS") is { Length: > 0 } e
            ? e
            : Path.Combine(RepoPaths.Root, "tests", "corpus", "ntfs", "generated");
        var p = Path.Combine(dir, name);
        Assert.True(File.Exists(p), $"falta {p}: descargue el corpus con python -I -X utf8 tools/fetch_ntfs_corpus.py");
        return p;
    }

    /// <summary>Campos de la entrada <c>[[files]]</c> con la ruta dada.</summary>
    private static Dictionary<string, string> ManifestFile(string name, string path)
    {
        var files = new List<Dictionary<string, string>>();
        Dictionary<string, string>? cur = null;
        foreach (var l in File.ReadAllLines(Path.Combine(RepoPaths.Root, "tests", "corpus", "ntfs", "manifests", name)).Select(x => x.Trim()))
        {
            if (l.StartsWith('['))
            {
                cur = l == "[[files]]" ? [] : null;
                if (cur is not null)
                {
                    files.Add(cur);
                }
                continue;
            }
            if (cur is not null && l.Split(" = ", 2) is [var k, var v])
            {
                cur[k] = v.Trim('"');
            }
        }
        return files.Single(f => f["path"] == path);
    }

    private static List<AutomationElement> Items(AppSession app, string listId) =>
        app.ById(listId).FindAll(TreeScope.Children, new OrCondition(
                new PropertyCondition(AutomationElement.ControlTypeProperty, ControlType.ListItem),
                new PropertyCondition(AutomationElement.ControlTypeProperty, ControlType.DataItem)))
            .Cast<AutomationElement>().ToList();

    private static void Select(AutomationElement e) =>
        ((SelectionItemPattern)e.GetCurrentPattern(SelectionItemPattern.Pattern)).Select();

    private static void Expand(AutomationElement e) =>
        ((ExpandCollapsePattern)e.GetCurrentPattern(ExpandCollapsePattern.Pattern)).Expand();

    private static void Toggle(AutomationElement e) =>
        ((TogglePattern)e.GetCurrentPattern(TogglePattern.Pattern)).Toggle();

    [Fact]
    public void Muestra_los_borrados_y_recupera_uno_con_el_sha256_del_manifiesto()
    {
        const string manifest = "ntfs_deleted.manifest.toml";
        var image = CorpusImage("ntfs_deleted.img");
        var docs = ManifestFile(manifest, "/docs");
        var cartas = ManifestFile(manifest, "/docs/cartas");
        var carta = ManifestFile(manifest, "/docs/cartas/carta_03.txt");
        var sello = ManifestFile(manifest, "/docs/cartas/sello.bin");
        Assert.Equal("deleted", cartas["state"]);
        Assert.Equal("full", carta["recoverable"]);
        var before = FileFingerprint.Of(image);
        var outDir = Directory.CreateTempSubdirectory("memrescue-uitests-borrados-").FullName;
        try
        {
            using (var app = AppSession.Launch(image))
            {
                ((InvokePattern)app.OpenMenuItem("DiskMenu", "OpenFileSystemMenuItem").GetCurrentPattern(InvokePattern.Pattern)).Invoke();
                app.WaitUntil(() => app.ById("FsVolumeText").Current.Name.StartsWith("NTFS «MR_DELETED»", StringComparison.Ordinal),
                    "los datos del volumen");
                app.WaitUntil(() => app.ById("FsStatusText").Current.Name.StartsWith("/: ", StringComparison.Ordinal), "el listado de la raíz");
                Assert.False(app.ById("FsRecoverButton").Current.IsEnabled, "sin borrado seleccionado no se recupera nada");
                var toggle = app.ById("FsShowDeletedToggle");
                app.WaitUntil(() => toggle.Current.IsEnabled, "«Mostrar borrados» habilitado");

                // Activar: la búsqueda termina y la raíz se vuelve a listar con los borrados.
                Toggle(toggle);
                app.WaitUntil(() => app.ById("FsStatusText").Current.Name.StartsWith("/: ", StringComparison.Ordinal)
                    && app.ById("FsStatusText").Current.Name.Contains("borrados mostrados", StringComparison.Ordinal),
                    "la raíz con los borrados", TimeSpan.FromSeconds(60));
                Assert.Equal(ToggleState.On, ((TogglePattern)toggle.GetCurrentPattern(TogglePattern.Pattern)).Current.ToggleState);

                // /docs (vivo) contiene el directorio borrado /docs/cartas.
                Expand(app.ById("FsDir5"));
                var docsItem = app.ById("FsDir" + docs["mft_record"]);
                Expand(docsItem);
                var cartasItem = app.ById("FsDeletedDir" + cartas["mft_record"]);
                Assert.Equal("cartas (borrado)", cartasItem.Current.Name);
                Select(cartasItem);
                app.WaitUntil(() => app.ById("FsStatusText").Current.Name.StartsWith("/docs/cartas (DIRECTORIO BORRADO): 5 borrados", StringComparison.Ordinal),
                    "los borrados de /docs/cartas");

                // sello.bin: encontrado en la MFT y en el índice del directorio borrado.
                var selloRow = app.ById("FsDeleted" + sello["mft_record"]);
                Assert.Contains("BORRADO, recuperable: íntegro", selloRow.Current.Name, StringComparison.Ordinal);
                Assert.Contains(Items(app, "FsFileList"), i => i.Current.AutomationId == "FsDeleted" + sello["mft_record"]);

                var row = app.ById("FsDeleted" + carta["mft_record"]);
                Assert.StartsWith($"carta_03.txt: {carta["size"]} bytes, registro {carta["mft_record"]} (secuencia {carta["sequence"]})",
                    row.Current.Name, StringComparison.Ordinal);
                Select(row);
                app.WaitUntil(() => app.ById("FsRecoverButton").Current.IsEnabled, "«Recuperar…» habilitado");
                Assert.False(app.ById("FsSaveAsButton").Current.IsEnabled, "un borrado no se copia como un vivo");

                // Recuperar… en un directorio temporal (nunca junto al origen).
                var output = Path.Combine(outDir, "carta recuperada.txt");
                app.Invoke("FsRecoverButton");
                var dlg = app.WaitForDialog("Recuperar fichero borrado como");
                var name = AppSession.WaitFor(() => dlg.FindFirst(TreeScope.Descendants,
                    new AndCondition(new PropertyCondition(AutomationElement.AutomationIdProperty, "1001"),
                        new PropertyCondition(AutomationElement.ControlTypeProperty, ControlType.Edit))), "el nombre del fichero");
                ((ValuePattern)name.GetCurrentPattern(ValuePattern.Pattern)).SetValue(output);
                var save = AppSession.WaitFor(() => dlg.FindFirst(TreeScope.Descendants,
                    new AndCondition(new PropertyCondition(AutomationElement.AutomationIdProperty, "1"),
                        new PropertyCondition(AutomationElement.ControlTypeProperty, ControlType.Button))), "el botón Guardar");
                ((InvokePattern)save.GetCurrentPattern(InvokePattern.Pattern)).Invoke();
                app.WaitUntil(() => app.ById("FsStatusText").Current.Name.StartsWith("Recuperado «carta_03.txt»", StringComparison.Ordinal),
                    "la recuperación terminada");
                Assert.Equal(carta["sha256"], Convert.ToHexStringLower(SHA256.HashData(File.ReadAllBytes(output))));
                Assert.Equal([output], Directory.GetFiles(outDir));

                // Desactivar: el árbol se rehace sin borrados.
                Toggle(app.ById("FsShowDeletedToggle"));
                app.WaitUntil(() => app.ById("FsStatusText").Current.Name.StartsWith("/: ", StringComparison.Ordinal)
                    && !app.ById("FsStatusText").Current.Name.Contains("borrados mostrados", StringComparison.Ordinal),
                    "la raíz sin borrados");
                Assert.DoesNotContain(Items(app, "FsFileList"), i => i.Current.AutomationId.StartsWith("FsDeleted", StringComparison.Ordinal));
                Assert.True(app.CloseGracefully(), "MemRescue debe cerrarse limpiamente (código 0).");
            }
            Assert.Equal(before, FileFingerprint.Of(image));
        }
        finally
        {
            Directory.Delete(outDir, recursive: true);
        }
    }
}
