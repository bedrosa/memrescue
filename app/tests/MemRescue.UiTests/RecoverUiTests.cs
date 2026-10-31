using System.IO;
using System.Security.Cryptography;
using System.Windows.Automation;
using Xunit;

namespace MemRescue.UiTests;

/// <summary>
/// Recuperación (#26): en la pestaña «Recuperación» se carga el árbol virtual de <c>ntfs_basic.img</c>
/// (corpus NTFS), se marca la carpeta <c>/docs</c>, se recupera en un directorio temporal y se comprueban
/// los SHA-256 del manifiesto, el resumen, el diario y que el origen no cambia.
/// </summary>
[Collection(UiCollection.Name)]
[Trait("Category", "NtfsCorpus")]
public sealed class RecoverUiTests
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

    private static List<Dictionary<string, string>> ManifestFiles(string name)
    {
        var files = new List<Dictionary<string, string>>();
        Dictionary<string, string>? cur = null;
        foreach (var l in File.ReadAllLines(Path.Combine(RepoPaths.Root, "tests", "corpus", "ntfs", "manifests", name)).Select(x => x.Trim()))
        {
            if (l.StartsWith('['))
            {
                cur = null;
                if (l == "[[files]]")
                {
                    cur = [];
                    files.Add(cur);
                }
                continue;
            }
            if (cur is not null && l.Split(" = ", 2) is [var k, var v])
            {
                cur[k] = v.Trim('"');
            }
        }
        return files;
    }

    private static void Select(AutomationElement e) =>
        ((SelectionItemPattern)e.GetCurrentPattern(SelectionItemPattern.Pattern)).Select();

    private static void Expand(AutomationElement e) =>
        ((ExpandCollapsePattern)e.GetCurrentPattern(ExpandCollapsePattern.Pattern)).Expand();

    [Fact]
    public void Recupera_una_carpeta_del_arbol_virtual_con_sus_hashes()
    {
        var image = CorpusImage("ntfs_basic.img");
        var before = FileFingerprint.Of(image);
        var outDir = Directory.CreateTempSubdirectory("memrescue-uitests-recover-").FullName;
        var dest = Path.Combine(outDir, "recuperado");
        try
        {
            using (var app = AppSession.Launch(image))
            {
                Select(app.ById("RecoverTab"));
                app.WaitUntil(() => app.ById("RecoverLoadTreeButton").Current.IsEnabled, "«Cargar árbol» habilitado");
                Assert.False(app.ById("RecoverStartButton").Current.IsEnabled, "sin árbol no se recupera");
                app.Invoke("RecoverLoadTreeButton");
                app.WaitUntil(() => app.ById("RecoverTreeText").Current.Name.Contains("ficheros", StringComparison.Ordinal),
                    "el árbol virtual construido", TimeSpan.FromSeconds(60));
                app.WaitUntil(() => ((SelectionItemPattern)app.ById("RecoverTab").GetCurrentPattern(SelectionItemPattern.Pattern)).Current.IsSelected,
                    "la pestaña «Recuperación» seleccionada");
                Expand(app.ById("RecoverNode0"));
                var docs = AppSession.WaitFor(() => app.ById("RecoverTree").FindFirst(TreeScope.Descendants,
                    new AndCondition(
                        new PropertyCondition(AutomationElement.ControlTypeProperty, ControlType.CheckBox),
                        new PropertyCondition(AutomationElement.NameProperty, "/docs: docs"))), "la casilla de /docs");
                ((TogglePattern)docs.GetCurrentPattern(TogglePattern.Pattern)).Toggle();
                app.SetText("RecoverDestBox", dest);
                app.WaitUntil(() => app.ById("RecoverStartButton").Current.IsEnabled, "«Recuperar» habilitado");
                app.Invoke("RecoverStartButton");
                app.WaitUntil(() => app.ById("RecoverSummaryText").Current.Name.StartsWith("Completa", StringComparison.Ordinal),
                    "la recuperación terminada", TimeSpan.FromSeconds(60));
                var summary = app.ById("RecoverSummaryText").Current.Name;
                Assert.StartsWith("Completa: todo lo escrito es íntegro.", summary, StringComparison.Ordinal);
                var journal = ((ValuePattern)app.ById("RecoverJournalBox").GetCurrentPattern(ValuePattern.Pattern)).Current.Value;
                Assert.True(File.Exists(journal), journal);
                Assert.StartsWith(dest, journal, StringComparison.OrdinalIgnoreCase);
                Assert.True(app.ById("RecoverOpenJournalButton").Current.IsEnabled);
                Assert.True(app.CloseGracefully(), "MemRescue debe cerrarse limpiamente (código 0).");
            }
            var docsFiles = ManifestFiles("ntfs_basic.manifest.toml")
                .Where(f => f["type"] == "file" && f["state"] == "live" && f["path"].StartsWith("/docs/", StringComparison.Ordinal))
                .ToList();
            Assert.True(docsFiles.Count >= 10);
            foreach (var f in docsFiles)
            {
                var p = Path.Combine(dest, f["path"].TrimStart('/').Replace('/', Path.DirectorySeparatorChar));
                Assert.Equal(f["sha256"], Convert.ToHexStringLower(SHA256.HashData(File.ReadAllBytes(p))));
            }
            // Solo /docs (y el diario): nada fuera de la selección.
            Assert.Equal(["MemRescue-registro", "docs"],
                Directory.GetFileSystemEntries(dest).Select(p => Path.GetFileName(p)!).Order(StringComparer.Ordinal).ToArray());
            Assert.Empty(Directory.GetFiles(dest, "*.mrtmp", SearchOption.AllDirectories));
            Assert.Equal(before, FileFingerprint.Of(image));
        }
        finally
        {
            Directory.Delete(outDir, recursive: true);
        }
    }
}
