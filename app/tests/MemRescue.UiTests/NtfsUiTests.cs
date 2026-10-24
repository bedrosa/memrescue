using System.Globalization;
using System.IO;
using System.Security.Cryptography;
using System.Windows.Automation;
using Xunit;

namespace MemRescue.UiTests;

/// <summary>
/// Sistema de archivos NTFS (#22): desde la pestaña Particiones se abre el sistema de archivos de la
/// partición de <c>ntfs_mbr.img</c> (corpus NTFS), se navega por el árbol hasta un fichero, se guarda
/// en un directorio temporal (SHA-256 del manifiesto) y se abre en el visor.
/// </summary>
[Collection(UiCollection.Name)]
[Trait("Category", "NtfsCorpus")]
public sealed class NtfsUiTests
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

    /// <summary>Campos de la entrada <c>[[files]]</c> con la ruta dada y los de <c>[volume]</c>.</summary>
    private static (Dictionary<string, string> File, Dictionary<string, string> Volume) Manifest(string name, string path)
    {
        var lines = File.ReadAllLines(Path.Combine(RepoPaths.Root, "tests", "corpus", "ntfs", "manifests", name));
        var volume = new Dictionary<string, string>();
        var files = new List<Dictionary<string, string>>();
        Dictionary<string, string>? cur = null;
        foreach (var l in lines.Select(x => x.Trim()))
        {
            if (l.StartsWith('['))
            {
                cur = l switch
                {
                    "[volume]" => volume,
                    "[[files]]" => [],
                    _ => null,
                };
                if (l == "[[files]]")
                {
                    files.Add(cur!);
                }
                continue;
            }
            if (cur is not null && l.Split(" = ", 2) is [var k, var v])
            {
                cur[k] = v.Trim('"');
            }
        }
        return (files.Single(f => f["path"] == path), volume);
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

    [Fact]
    public void Abre_el_sistema_de_archivos_de_una_particion_navega_guarda_y_abre_un_fichero()
    {
        const string target = "/docs/proyectos/alfa/diseño.bin";
        var image = CorpusImage("ntfs_mbr.img");
        var (file, volume) = Manifest("ntfs_mbr.manifest.toml", target);
        var record = file["mft_record"];
        var before = FileFingerprint.Of(image);
        var outDir = Directory.CreateTempSubdirectory("memrescue-uitests-ntfs-").FullName;
        try
        {
            using (var app = AppSession.Launch(image))
            {
                Select(app.ById("PartitionsTab"));
                app.WaitUntil(() => Items(app, "PartitionsList").Count == 1, "la partición NTFS de ntfs_mbr");
                Assert.False(app.ById("OpenPartitionFileSystemButton").Current.IsEnabled, "sin selección no se abre nada");
                Select(app.ById("Partition1"));
                app.WaitUntil(() => app.ById("OpenPartitionFileSystemButton").Current.IsEnabled, "«Abrir sistema de archivos» habilitado");
                app.Invoke("OpenPartitionFileSystemButton");
                app.WaitUntil(() => ((SelectionItemPattern)app.ById("FileSystemTab").GetCurrentPattern(SelectionItemPattern.Pattern)).Current.IsSelected,
                    "la pestaña «Sistema de archivos» seleccionada");
                app.WaitUntil(() => app.ById("FsVolumeText").Current.Name.StartsWith("NTFS «MR_MBR»", StringComparison.Ordinal),
                    "los datos del volumen");
                // Raíz: la lista muestra sus entradas con el nombre acentuado.
                app.WaitUntil(() => Items(app, "FsFileList").Any(i => i.Current.Name.StartsWith("configuración.ini: 300 bytes", StringComparison.Ordinal)),
                    "las entradas de la raíz");
                Assert.Contains("/: ", app.ById("FsStatusText").Current.Name, StringComparison.Ordinal);

                // Navegación por el árbol: /docs → proyectos → alfa.
                var path = "";
                foreach (var dir in new[] { "docs", "proyectos", "alfa" })
                {
                    path += "/" + dir;
                    var (d, _) = Manifest("ntfs_mbr.manifest.toml", path);
                    var parentItem = app.ById(path.Count(c => c == '/') == 1 ? "FsDir5" : "FsDir" + Manifest("ntfs_mbr.manifest.toml", path[..path.LastIndexOf('/')]).File["mft_record"]);
                    Expand(parentItem);
                    var item = app.ById("FsDir" + d["mft_record"]);
                    Assert.Equal(dir, item.Current.Name);
                    Select(item);
                }
                app.WaitUntil(() => app.ById("FsStatusText").Current.Name.StartsWith("/docs/proyectos/alfa: 5 entradas, 5 vivas", StringComparison.Ordinal),
                    "el listado de /docs/proyectos/alfa");
                var row = app.ById("FsEntry" + record);
                Assert.StartsWith($"diseño.bin: {file["size"]} bytes, registro {record}", row.Current.Name, StringComparison.Ordinal);
                Select(row);
                app.WaitUntil(() => app.ById("FsSaveAsButton").Current.IsEnabled, "«Guardar como…» habilitado");

                // Guardar como… en un directorio temporal (nunca junto al origen).
                var output = Path.Combine(outDir, "diseño copia.bin");
                app.Invoke("FsSaveAsButton");
                var dlg = app.WaitForDialog("Guardar fichero como");
                var name = AppSession.WaitFor(() => dlg.FindFirst(TreeScope.Descendants,
                    new AndCondition(new PropertyCondition(AutomationElement.AutomationIdProperty, "1001"),
                        new PropertyCondition(AutomationElement.ControlTypeProperty, ControlType.Edit))), "el nombre del fichero");
                ((ValuePattern)name.GetCurrentPattern(ValuePattern.Pattern)).SetValue(output);
                // Botón «Guardar» (id 1): la lista de ficheros del cuadro también tiene elementos con id 1.
                var save = AppSession.WaitFor(() => dlg.FindFirst(TreeScope.Descendants,
                    new AndCondition(new PropertyCondition(AutomationElement.AutomationIdProperty, "1"),
                        new PropertyCondition(AutomationElement.ControlTypeProperty, ControlType.Button))), "el botón Guardar");
                ((InvokePattern)save.GetCurrentPattern(InvokePattern.Pattern)).Invoke();
                app.WaitUntil(() => app.ById("FsStatusText").Current.Name.StartsWith("Guardado «diseño.bin»", StringComparison.Ordinal),
                    "la copia terminada");
                Assert.Equal(file["sha256"], Convert.ToHexStringLower(SHA256.HashData(File.ReadAllBytes(output))));
                Assert.Equal([output], Directory.GetFiles(outDir));

                // Abrir en el visor: el fichero es un origen nuevo; su LBA 0 son sus primeros bytes.
                app.Invoke("FsOpenInViewerButton");
                app.WaitUntil(() => Items(app, "SourcesList").Count == 3, "el fichero como tercer origen");
                var runs = file["runs"].Trim('[', ']').Split(',').Select(x => ulong.Parse(x.Trim(' ', '['), CultureInfo.InvariantCulture)).ToArray();
                var at = ulong.Parse(volume["offset"], CultureInfo.InvariantCulture) + runs[1] * ulong.Parse(volume["cluster_size"], CultureInfo.InvariantCulture);
                var first = new byte[16];
                using (var fs = new FileStream(image, FileMode.Open, FileAccess.Read, FileShare.ReadWrite))
                {
                    fs.Seek((long)at, SeekOrigin.Begin);
                    fs.ReadExactly(first);
                }
                var hex = string.Join(" ", first.Take(8).Select(b => b.ToString("X2", CultureInfo.InvariantCulture)));
                app.WaitUntil(() => app.HexText.Split('\n').Any(l => l.StartsWith("000000000000  " + hex, StringComparison.Ordinal)),
                    "el primer sector del fichero en el visor");
                Assert.Equal(0UL, app.ShownLba);
                var texts = Items(app, "SourcesList")[2].FindAll(TreeScope.Descendants,
                        new PropertyCondition(AutomationElement.ControlTypeProperty, ControlType.Text))
                    .Cast<AutomationElement>().Select(e => e.Current.Name).ToList();
                Assert.Contains(texts, t => t.StartsWith($"diseño.bin [registro {record} de ntfs_mbr.img [partición 1", StringComparison.Ordinal));
                // 100000 bytes = 196 sectores completos más uno parcial.
                app.Invoke("LastSectorButton");
                app.WaitForLba(195);
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
