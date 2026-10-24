using System.Globalization;
using System.IO;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using System.Windows.Input;
using MemRescue.Core.Interop;
using Microsoft.Win32;

namespace MemRescue;

/// <summary>Fila de la lista de ficheros de un directorio NTFS (textos ya formateados).</summary>
public sealed class FsEntryRow(NtfsEntry entry)
{
    public NtfsEntry Entry { get; } = entry;

    public string Name => Entry.Name;

    public string SizeText => Entry.IsDirectory ? "<DIR>" : Entry.Size.ToString("N0", CultureInfo.CurrentCulture);

    private static string Date(DateTime? d) => d?.ToString("yyyy-MM-dd HH:mm:ss", CultureInfo.InvariantCulture) ?? "";

    public string ModifiedText => Date(Entry.Modified);

    public string CreatedText => Date(Entry.Created);

    public string RecordText => $"{Entry.Record}-{Entry.Sequence}";

    public string StateText => Entry.State.Label() + (Entry.IsSystem ? " · sistema" : "");

    public string AutomationId => "FsEntry" + Entry.Record.ToString(CultureInfo.InvariantCulture);

    public string AutomationName =>
        $"{Entry.Name}: {(Entry.IsDirectory ? "directorio" : $"{Entry.Size} bytes")}, registro {Entry.Record} (secuencia {Entry.Sequence}), " +
        $"modificado {ModifiedText}, {Entry.State.Label()}";
}

public partial class MainWindow
{
    /// <summary>Último listado pedido (los anteriores que terminen después se descartan).</summary>
    private int _fsRequest;

    private CancellationTokenSource? _copyCts;

    private SourceEntry? FsSource => Selected is { Ntfs: not null } s ? s : null;

    private void ShowFileSystemTab_Click(object sender, RoutedEventArgs e) => MainTabs.SelectedItem = FileSystemTab;

    private void OpenFileSystem_Executed(object sender, ExecutedRoutedEventArgs e)
    {
        if (Selected is { } s)
        {
            _ = OpenFileSystemAsync(s);
        }
    }

    /// <summary>Abre (una sola vez) el sistema de archivos NTFS del origen y lo muestra.</summary>
    internal async Task<NtfsVolume?> OpenFileSystemAsync(SourceEntry s)
    {
        if (s.Ntfs is null)
        {
            try
            {
                StatusText.Text = "Abriendo el sistema de archivos…";
                var v = await Task.Run(s.Source.OpenNtfs);
                if (!_sources.Contains(s) || s.Ntfs is not null)
                {
                    v.Dispose();
                }
                else
                {
                    s.Ntfs = v;
                    var i = v.Info;
                    Log($"Sistema de archivos NTFS de {s.FileName}: «{i.Label}», {i.RecordCount:N0} registros MFT, clúster de {i.ClusterSize} B, " +
                        $"confianza {i.Confidence.Label()}{(i.UpcaseFromVolume ? "" : ", mayúsculas por plegado simple ($UpCase no utilizable)")}.");
                    foreach (var a in i.Anomalies)
                    {
                        Log($"  Volumen: {a}");
                    }
                }
            }
            catch (EngineException ex)
            {
                ShowError("No se pudo abrir el sistema de archivos", ex);
                return null;
            }
            finally
            {
                StatusText.Text = "Listo";
            }
        }
        if (Selected == s)
        {
            ShowFileSystem(s);
        }
        MainTabs.SelectedItem = FileSystemTab;
        return s.Ntfs;
    }

    private async void OpenPartitionFileSystem_Click(object sender, RoutedEventArgs e)
    {
        if (Selected is { } s && PartitionsList.SelectedItem is PartitionRow row
            && await OpenPartitionAsync(s, row.Entry) is { } entry)
        {
            await OpenFileSystemAsync(entry);
        }
    }

    private async void OpenFoundFileSystem_Click(object sender, RoutedEventArgs e)
    {
        if (Selected is { } s && FoundVolumesList.SelectedItem is FoundVolumeRow row
            && await OpenFoundVolumeAsync(s, row.Volume) is { } entry)
        {
            await OpenFileSystemAsync(entry);
        }
    }

    /// <summary>Muestra el sistema de archivos del origen (o el aviso de que no está abierto).</summary>
    private void ShowFileSystem(SourceEntry? s)
    {
        Interlocked.Increment(ref _fsRequest);
        FsTree.Items.Clear();
        FsFileList.ItemsSource = null;
        FsOpenInViewerButton.IsEnabled = false;
        FsSaveAsButton.IsEnabled = false;
        FsStatusText.Text = "";
        if (s?.Ntfs is not { } v)
        {
            FsGoButton.IsEnabled = false;
            FsVolumeText.Text = s is null
                ? "Abra una imagen de disco y después su sistema de archivos NTFS."
                : "El sistema de archivos de este origen no está abierto: Disco → Abrir sistema de archivos NTFS del origen " +
                  "(o, en la pestaña Particiones, «Abrir sistema de archivos» sobre una partición o un volumen encontrado).";
            return;
        }
        var i = v.Info;
        FsGoButton.IsEnabled = true;
        FsVolumeText.Text =
            $"NTFS «{i.Label}» · serie {i.Serial:X16}" +
            (i.Version is { } ver ? $" · versión {ver.Major}.{ver.Minor}" : "") +
            $" · clúster {i.ClusterSize} B · {i.RecordCount:N0} registros MFT · confianza {i.Confidence.Label()}" +
            (i.BootFromBackup ? " · ARRANQUE DESDE LA COPIA" : "") +
            (i.UpcaseFromVolume ? "" : " · mayúsculas: plegado simple") +
            ". Solo lectura.";
        var root = DirItem(NtfsVolume.RootRecord, "/");
        FsTree.Items.Add(root);
        root.IsExpanded = true;
        root.IsSelected = true;
    }

    private TreeViewItem DirItem(ulong record, string name)
    {
        var item = new TreeViewItem { Header = name, Tag = record };
        AutomationProperties.SetAutomationId(item, "FsDir" + record.ToString(CultureInfo.InvariantCulture));
        AutomationProperties.SetName(item, name);
        item.Items.Add("…");
        item.Expanded += FsDir_Expanded;
        return item;
    }

    /// <summary>Listado del directorio (en segundo plano, con caché por origen).</summary>
    private async Task<NtfsDirectoryListing?> ListAsync(SourceEntry s, ulong record)
    {
        if (s.FsListings.TryGetValue(record, out var cached))
        {
            return cached;
        }
        if (s.Ntfs is not { } v)
        {
            return null;
        }
        try
        {
            var l = await Task.Run(() => v.ListDirectory(record));
            s.FsListings[record] = l;
            foreach (var a in l.Anomalies)
            {
                Log($"Directorio {record} de {s.FileName}: {a}");
            }
            return l;
        }
        catch (EngineException ex)
        {
            Log($"No se pudo listar el directorio {record} de {s.FileName}: [{ex.Status}] {ex.Message}");
            FsStatusText.Text = $"No se pudo listar el directorio (registro {record}): {ex.Message}";
            return null;
        }
    }

    /// <summary>Al desplegar un directorio se cargan sus subdirectorios vivos.</summary>
    private async void FsDir_Expanded(object sender, RoutedEventArgs e)
    {
        if (e.OriginalSource is TreeViewItem item && item.Items.Count == 1 && item.Items[0] is string)
        {
            await LoadChildrenAsync(item);
        }
    }

    private async Task LoadChildrenAsync(TreeViewItem item)
    {
        if (FsSource is not { } s || item.Tag is not ulong record || item.Items.Count != 1 || item.Items[0] is not string)
        {
            return;
        }
        var l = await ListAsync(s, record);
        if (l is null || item.Items.Count != 1 || item.Items[0] is not string)
        {
            return;
        }
        item.Items.Clear();
        foreach (var d in l.Entries.Where(x => x.IsLive && x.IsDirectory))
        {
            item.Items.Add(DirItem(d.Record, d.Name));
        }
    }

    private void FsTree_SelectedItemChanged(object sender, RoutedPropertyChangedEventArgs<object> e)
    {
        if (e.NewValue is TreeViewItem { Tag: ulong record } && FsSource is { } s)
        {
            _ = ShowDirectoryAsync(s, record);
        }
    }

    /// <summary>Muestra las entradas del directorio en la lista (vivas y residuales, con su estado).</summary>
    internal async Task<NtfsDirectoryListing?> ShowDirectoryAsync(SourceEntry s, ulong record)
    {
        var request = Interlocked.Increment(ref _fsRequest);
        FsStatusText.Text = "Leyendo el directorio…";
        var l = await ListAsync(s, record);
        if (request != _fsRequest || FsSource != s || l is null)
        {
            return l;
        }
        string path;
        try
        {
            path = await Task.Run(() => s.Ntfs!.PathOf(record));
        }
        catch (EngineException)
        {
            path = $"(registro {record})";
        }
        if (request != _fsRequest)
        {
            return l;
        }
        FsFileList.ItemsSource = l.Entries.Select(x => new FsEntryRow(x)).ToList();
        FsPathBox.Text = path;
        var notLive = l.Entries.Count(x => !x.IsLive);
        FsStatusText.Text = $"{path}: {l.Entries.Count} entradas, {l.Entries.Count - notLive} vivas" +
            (notLive > 0 ? $", {notLive} NO vivas (residuales o con el registro dañado: no son ficheros vivos)" : "") +
            (l.Anomalies.Count > 0 ? $" · {l.Anomalies.Count} anomalías del índice (ver el registro de actividad)" : "") +
            (l.Unreadable ? " · HAY NODOS O REGISTROS ILEGIBLES" : "");
        return l;
    }

    private FsEntryRow? SelectedFsRow => FsFileList.SelectedItem as FsEntryRow;

    private void FsFileList_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        var file = SelectedFsRow is { Entry: { IsLive: true, IsDirectory: false } };
        FsOpenInViewerButton.IsEnabled = file;
        FsSaveAsButton.IsEnabled = file && _copyCts is null;
    }

    private async void FsFileList_MouseDoubleClick(object sender, MouseButtonEventArgs e)
    {
        if (SelectedFsRow is not { Entry: { IsLive: true } entry })
        {
            return;
        }
        if (entry.IsDirectory)
        {
            await SelectDirectoryAsync(entry.Record);
        }
        else if (FsSource is { } s)
        {
            await OpenFileInViewerAsync(s, entry);
        }
    }

    /// <summary>Selecciona en el árbol el subdirectorio del directorio seleccionado.</summary>
    private async Task SelectDirectoryAsync(ulong record)
    {
        if (FsTree.SelectedItem is not TreeViewItem current)
        {
            return;
        }
        await LoadChildrenAsync(current);
        current.IsExpanded = true;
        if (current.Items.OfType<TreeViewItem>().FirstOrDefault(i => i.Tag is ulong r && r == record) is { } child)
        {
            child.IsSelected = true;
            child.BringIntoView();
        }
    }

    private void FsPathBox_KeyDown(object sender, KeyEventArgs e)
    {
        if (e.Key == Key.Enter)
        {
            FsGo_Click(sender, e);
            e.Handled = true;
        }
    }

    private void FsGo_Click(object sender, RoutedEventArgs e)
    {
        if (FsSource is { } s)
        {
            _ = GoToPathAsync(s, FsPathBox.Text);
        }
    }

    /// <summary>
    /// Va a una ruta (sin distinguir mayúsculas): despliega el árbol hasta su directorio y, si es un
    /// fichero, lo selecciona en la lista.
    /// </summary>
    internal async Task<NtfsEntry?> GoToPathAsync(SourceEntry s, string path)
    {
        var v = s.Ntfs!;
        NtfsEntry target;
        string canonical;
        try
        {
            target = await Task.Run(() => v.Resolve(path.Trim().Length == 0 ? "/" : path.Trim()));
            var dir = target.IsDirectory ? target.Record : target.ParentRecord;
            canonical = await Task.Run(() => v.PathOf(dir));
        }
        catch (EngineException ex)
        {
            FsStatusText.Text = ex.Status == EngineStatus.NotFound ? $"No existe: {ex.Message}" : $"Ruta no válida: {ex.Message}";
            System.Media.SystemSounds.Beep.Play();
            return null;
        }
        if (FsTree.Items.Count == 0 || FsTree.Items[0] is not TreeViewItem item)
        {
            return null;
        }
        foreach (var name in canonical.Split('/', StringSplitOptions.RemoveEmptyEntries))
        {
            await LoadChildrenAsync(item);
            item.IsExpanded = true;
            if (item.Items.OfType<TreeViewItem>().FirstOrDefault(i => (string)i.Header == name) is not { } next)
            {
                FsStatusText.Text = $"No se encuentra «{name}» en el árbol.";
                return null;
            }
            item = next;
        }
        item.IsSelected = true;
        item.BringIntoView();
        await ShowDirectoryAsync(s, (ulong)item.Tag);
        if (!target.IsDirectory && FsFileList.ItemsSource is IEnumerable<FsEntryRow> rows
            && rows.FirstOrDefault(r => r.Entry.Record == target.Record) is { } row)
        {
            FsFileList.SelectedItem = row;
            FsFileList.ScrollIntoView(row);
        }
        return target;
    }

    private void FsOpenInViewer_Click(object sender, RoutedEventArgs e)
    {
        if (FsSource is { } s && SelectedFsRow is { Entry: { IsLive: true, IsDirectory: false } entry })
        {
            _ = OpenFileInViewerAsync(s, entry);
        }
    }

    /// <summary>Abre el fichero como un origen de solo lectura y lo muestra en el visor de sectores.</summary>
    internal async Task<SourceEntry?> OpenFileInViewerAsync(SourceEntry s, NtfsEntry entry)
    {
        try
        {
            var src = await Task.Run(() => s.Ntfs!.OpenStream(entry.Record));
            var e = new SourceEntry(src, ImageFormat.Raw, false, $"{entry.Name} [registro {entry.Record} de {s.FileName}]", s.Tolerant);
            AddSource(e);
            Log($"Fichero abierto en el visor: {src.Description}.");
            return e;
        }
        catch (EngineException ex)
        {
            ShowError("No se pudo abrir el fichero", ex);
            return null;
        }
    }

    private void FsSaveAs_Click(object sender, RoutedEventArgs e)
    {
        if (FsSource is not { } s || SelectedFsRow is not { Entry: { IsLive: true, IsDirectory: false } entry })
        {
            return;
        }
        // El cuadro se abre de forma asíncrona para no bloquear la pulsación (UI Automation).
        Dispatcher.BeginInvoke(async () =>
        {
            var dlg = new SaveFileDialog
            {
                Title = "Guardar fichero como",
                FileName = entry.Name,
                Filter = "Todos los ficheros (*.*)|*.*",
                OverwritePrompt = true,
                AddExtension = false,
            };
            if (dlg.ShowDialog(this) != true)
            {
                return;
            }
            // El propio cuadro pidió confirmación si el destino existía.
            await SaveFileAsync(s, entry, dlg.FileName, overwrite: File.Exists(dlg.FileName));
        });
    }

    /// <summary>
    /// Copia el fichero a <paramref name="path"/> con la política del motor: nunca sobre el origen ni un
    /// dispositivo, sin sustituir un fichero existente salvo confirmación, escritura atómica.
    /// </summary>
    internal async Task<NtfsCopyReport?> SaveFileAsync(SourceEntry s, NtfsEntry entry, string path, bool overwrite)
    {
        if (_copyCts is not null)
        {
            return null;
        }
        _copyCts = new CancellationTokenSource();
        FsSaveAsButton.IsEnabled = false;
        try
        {
            using var task = s.Ntfs!.StartCopy(entry.Record, path, overwrite: overwrite);
            var progress = new Progress<TaskProgress>(p =>
                FsStatusText.Text = $"Guardando «{entry.Name}»… {p.Done:N0} de {p.Total:N0} bytes");
            var state = await task.WaitAsync(progress, _copyCts.Token);
            if (state == EngineTaskState.Cancelled)
            {
                FsStatusText.Text = "Copia cancelada: no se escribió ningún fichero.";
                return null;
            }
            var r = task.GetReport();
            FsStatusText.Text = r.AllTrustworthy
                ? $"Guardado «{entry.Name}» en {path} ({r.Bytes:N0} bytes)."
                : $"Guardado en {path} con {r.UntrustedBytes:N0} de {r.Bytes:N0} bytes NO FIABLES (patrón de relleno o posiblemente sobrescritos: no son datos fiables).";
            Log($"Fichero {entry.Name} (registro {entry.Record}) guardado en {path}: {r.Bytes:N0} bytes, {r.UntrustedBytes:N0} no fiables.");
            if (!r.AllTrustworthy)
            {
                MessageBox.Show(this,
                    $"{r.UntrustedBytes:N0} de {r.Bytes:N0} bytes no son datos fiables del fichero: " +
                    $"{r.UntrustedBytes - r.PossiblyOverwrittenBytes:N0} se escribieron con el patrón de relleno (ilegibles o sin mapa) y " +
                    $"{r.PossiblyOverwrittenBytes:N0} se leyeron de clústeres que pueden haberse reutilizado.",
                    "Copia con tramos no fiables", MessageBoxButton.OK, MessageBoxImage.Warning);
            }
            return r;
        }
        catch (EngineException ex)
        {
            FsStatusText.Text = $"No se pudo guardar: {ex.Message}";
            ShowError("No se pudo guardar el fichero", ex);
            return null;
        }
        finally
        {
            _copyCts.Dispose();
            _copyCts = null;
            FsFileList_SelectionChanged(FsFileList, null!);
        }
    }
}
