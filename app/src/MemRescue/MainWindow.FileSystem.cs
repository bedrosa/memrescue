using System.Globalization;
using System.IO;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using System.Windows.Input;
using MemRescue.Core.Interop;
using Microsoft.Win32;

namespace MemRescue;

/// <summary>
/// Fila de la lista de ficheros de un directorio NTFS (textos ya formateados). Con <paramref name="deleted"/>,
/// un borrado o una entrada residual (#23) con su recuperabilidad.
/// </summary>
public sealed class FsEntryRow(NtfsEntry entry, NtfsDeletedEntry? deleted = null)
{
    public NtfsEntry Entry { get; } = entry;

    public NtfsDeletedEntry? Deleted { get; } = deleted;

    public string Name => Entry.Name;

    public string SizeText => Entry.IsDirectory ? "<DIR>" : Entry.Size.ToString("N0", CultureInfo.CurrentCulture);

    private static string Date(DateTime? d) => d?.ToString("yyyy-MM-dd HH:mm:ss", CultureInfo.InvariantCulture) ?? "";

    public string ModifiedText => Date(Entry.Modified);

    public string CreatedText => Date(Entry.Created);

    public string RecordText => $"{Entry.Record}-{Entry.Sequence}";

    public string StateText => Deleted is { HasRecord: true } d
        ? $"BORRADO ({d.OriginsText})"
        : Entry.State.Label() + (Entry.IsSystem ? " · sistema" : "");

    public string RecoverableText => Deleted?.Recoverability.Label() ?? "";

    public string AutomationId => Deleted switch
    {
        null => "FsEntry" + Entry.Record.ToString(CultureInfo.InvariantCulture),
        { HasRecord: true } => "FsDeleted" + Entry.Record.ToString(CultureInfo.InvariantCulture),
        _ => FormattableString.Invariant($"FsResidual{Entry.Record}_{Entry.Sequence}"),
    };

    public string AutomationName =>
        $"{Entry.Name}: {(Entry.IsDirectory ? "directorio" : $"{Entry.Size} bytes")}, registro {Entry.Record} (secuencia {Entry.Sequence}), " +
        $"modificado {ModifiedText}, {(Deleted is { HasRecord: true } ? "BORRADO" : Entry.State.Label())}" +
        (Deleted is { } x ? $", recuperable: {x.Recoverability.Label()}" + (x.Summary.Length > 0 ? $" ({x.Summary})" : "") : "");
}

/// <summary>Elemento del árbol de un directorio borrado (#23).</summary>
internal sealed record FsDeletedDir(ulong Record);

/// <summary>Elemento del árbol con los borrados cuyo padre no se puede mostrar (huérfanos).</summary>
internal sealed record FsDeletedOrphans;

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
        FsRecoverButton.IsEnabled = false;
        FsStatusText.Text = "";
        _syncingToggle = true;
        FsShowDeletedToggle.IsChecked = s?.FsShowDeleted == true;
        _syncingToggle = false;
        if (s?.Ntfs is not { } v)
        {
            FsGoButton.IsEnabled = false;
            FsShowDeletedToggle.IsEnabled = false;
            FsVolumeText.Text = s is null
                ? "Abra una imagen de disco y después su sistema de archivos NTFS."
                : "El sistema de archivos de este origen no está abierto: Disco → Abrir sistema de archivos NTFS del origen " +
                  "(o, en la pestaña Particiones, «Abrir sistema de archivos» sobre una partición o un volumen encontrado).";
            return;
        }
        var i = v.Info;
        FsGoButton.IsEnabled = true;
        FsShowDeletedToggle.IsEnabled = _deletedScanCts is null;
        FsVolumeText.Text =
            $"NTFS «{i.Label}» · serie {i.Serial:X16}" +
            (i.Version is { } ver ? $" · versión {ver.Major}.{ver.Minor}" : "") +
            $" · clúster {i.ClusterSize} B · {i.RecordCount:N0} registros MFT · confianza {i.Confidence.Label()}" +
            (i.BootFromBackup ? " · ARRANQUE DESDE LA COPIA" : "") +
            (i.UpcaseFromVolume ? "" : " · mayúsculas: plegado simple") +
            ". Solo lectura.";
        var root = DirItem(NtfsVolume.RootRecord, "/");
        FsTree.Items.Add(root);
        if (ShowingDeleted(s) is { } scan && scan.Entries.Any(e => e.ParentState == NtfsParentState.Orphan))
        {
            var orphans = new TreeViewItem { Header = "<borrados sin ruta>", Tag = new FsDeletedOrphans() };
            AutomationProperties.SetAutomationId(orphans, "FsDeletedOrphans");
            AutomationProperties.SetName(orphans, "Borrados sin ruta (huérfanos)");
            FsTree.Items.Add(orphans);
        }
        root.IsExpanded = true;
        root.IsSelected = true;
    }

    /// <summary>La búsqueda de borrados si «Mostrar borrados» está activo en el origen.</summary>
    private static NtfsDeletedScan? ShowingDeleted(SourceEntry? s) => s is { FsShowDeleted: true, FsDeleted: { } d } ? d : null;

    private TreeViewItem DeletedDirItem(ulong record, string name)
    {
        var item = new TreeViewItem { Header = name + " (borrado)", Tag = new FsDeletedDir(record) };
        AutomationProperties.SetAutomationId(item, "FsDeletedDir" + record.ToString(CultureInfo.InvariantCulture));
        AutomationProperties.SetName(item, name + " (borrado)");
        item.Items.Add("…");
        item.Expanded += FsDir_Expanded;
        return item;
    }

    /// <summary>Directorios borrados (con su registro) dentro del directorio <paramref name="record"/>.</summary>
    private static IEnumerable<NtfsDeletedEntry> DeletedDirsIn(NtfsDeletedScan scan, ulong record) =>
        scan.Entries.Where(e => e.HasRecord && e.Entry.IsDirectory && e.Entry.ParentRecord == record
            && e.ParentState != NtfsParentState.Orphan);

    private CancellationTokenSource? _deletedScanCts;

    /// <summary><c>true</c> mientras el estado de «Mostrar borrados» se ajusta al del origen (no es una acción).</summary>
    private bool _syncingToggle;

    /// <summary>«Mostrar borrados»: busca los borrados (una vez por origen) y rehace el árbol.</summary>
    private async void FsShowDeleted_Changed(object sender, RoutedEventArgs e)
    {
        if (_syncingToggle || FsSource is not { } s || s.FsShowDeleted == (FsShowDeletedToggle.IsChecked == true))
        {
            return;
        }
        s.FsShowDeleted = FsShowDeletedToggle.IsChecked == true;
        if (s.FsShowDeleted && s.FsDeleted is null && !await ScanDeletedAsync(s))
        {
            s.FsShowDeleted = false;
        }
        if (FsSource == s)
        {
            ShowFileSystem(s);
        }
    }

    /// <summary>Búsqueda de borrados (MFT y entradas residuales de los índices) en segundo plano.</summary>
    internal async Task<bool> ScanDeletedAsync(SourceEntry s)
    {
        if (_deletedScanCts is not null || s.Ntfs is not { } v)
        {
            return false;
        }
        _deletedScanCts = new CancellationTokenSource();
        FsShowDeletedToggle.IsEnabled = false;
        try
        {
            using var task = v.StartDeletedScan();
            var progress = new Progress<TaskProgress>(p => FsStatusText.Text = $"Buscando borrados… {p.Done:N0} de {p.Total:N0}");
            if (await task.WaitAsync(progress, _deletedScanCts.Token) != EngineTaskState.Completed)
            {
                FsStatusText.Text = "Búsqueda de borrados cancelada.";
                return false;
            }
            var d = task.GetResult();
            s.FsDeleted = d;
            Log($"Borrados de {s.FileName}: {d.Entries.Count:N0} entradas ({d.ResidualOnlyCount:N0} solo en índices de directorio); " +
                $"índices: {d.IndexDirectories:N0} directorios, {d.IndexNodes:N0} nodos ({d.IndexFreeNodes:N0} libres), " +
                $"{d.ResidualAccepted:N0} entradas residuales aceptadas y {d.ResidualRejected:N0} restos rechazados" +
                (d.IndexUnreadable ? "; HAY NODOS DE ÍNDICE ILEGIBLES (no se interpretan)" : "") +
                (d.MftUnreadable ? "; HAY REGISTROS MFT ILEGIBLES" : "") +
                (d.BitmapUnusable ? "; $Bitmap NO UTILIZABLE: la asignación es desconocida" : "") + ".");
            return true;
        }
        catch (EngineException ex)
        {
            ShowError("No se pudieron buscar los borrados", ex);
            return false;
        }
        finally
        {
            _deletedScanCts.Dispose();
            _deletedScanCts = null;
            FsShowDeletedToggle.IsEnabled = FsSource?.Ntfs is not null;
        }
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
        if (FsSource is not { } s || item.Items.Count != 1 || item.Items[0] is not string)
        {
            return;
        }
        if (item.Tag is FsDeletedDir dd)
        {
            item.Items.Clear();
            if (ShowingDeleted(s) is { } scan)
            {
                foreach (var d in DeletedDirsIn(scan, dd.Record))
                {
                    item.Items.Add(DeletedDirItem(d.Entry.Record, d.Entry.Name));
                }
            }
            return;
        }
        if (item.Tag is not ulong record)
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
        if (ShowingDeleted(s) is { } deleted)
        {
            foreach (var d in DeletedDirsIn(deleted, record))
            {
                item.Items.Add(DeletedDirItem(d.Entry.Record, d.Entry.Name));
            }
        }
    }

    private void FsTree_SelectedItemChanged(object sender, RoutedPropertyChangedEventArgs<object> e)
    {
        if (e.NewValue is not TreeViewItem item || FsSource is not { } s)
        {
            return;
        }
        switch (item.Tag)
        {
            case ulong record:
                _ = ShowDirectoryAsync(s, record);
                break;
            case FsDeletedDir d:
                ShowDeletedOnly(s, d.Record);
                break;
            case FsDeletedOrphans:
                ShowDeletedOnly(s, null);
                break;
        }
    }

    /// <summary>Borrados de un directorio borrado (o los huérfanos con <c>null</c>): no hay listado vivo.</summary>
    private void ShowDeletedOnly(SourceEntry s, ulong? record)
    {
        Interlocked.Increment(ref _fsRequest);
        if (ShowingDeleted(s) is not { } scan)
        {
            return;
        }
        var rows = scan.Entries
            .Where(e => record is { } r ? e.Entry.ParentRecord == r && e.ParentState != NtfsParentState.Orphan : e.ParentState == NtfsParentState.Orphan)
            .Select(e => new FsEntryRow(e.Entry, e)).ToList();
        FsFileList.ItemsSource = rows;
        var path = record is { } rec
            ? scan.Entries.FirstOrDefault(e => e.Entry.Record == rec && e.HasRecord)?.Path ?? $"(registro {rec})"
            : "<borrados sin ruta>";
        FsPathBox.Text = path;
        FsStatusText.Text = record is null
            ? $"{rows.Count} borrados cuyo directorio no se puede mostrar (huérfanos: la ruta es <huérfano N>/nombre)."
            : $"{path} (DIRECTORIO BORRADO): {rows.Count} borrados.";
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
        var rows = l.Entries.Select(x => new FsEntryRow(x)).ToList();
        var deleted = new List<FsEntryRow>();
        if (ShowingDeleted(s) is { } scan)
        {
            deleted = scan.Entries.Where(e => e.Entry.ParentRecord == record && e.ParentState != NtfsParentState.Orphan)
                .Select(e => new FsEntryRow(e.Entry, e)).ToList();
            // Una entrada no viva del índice que ya es un borrado se muestra una vez (como borrado).
            var shown = deleted.Select(r => r.Entry.Record).ToHashSet();
            rows.RemoveAll(r => !r.Entry.IsLive && shown.Contains(r.Entry.Record));
            rows.AddRange(deleted);
        }
        FsFileList.ItemsSource = rows;
        FsPathBox.Text = path;
        var notLive = l.Entries.Count(x => !x.IsLive);
        FsStatusText.Text = $"{path}: {l.Entries.Count} entradas, {l.Entries.Count - notLive} vivas" +
            (notLive > 0 ? $", {notLive} NO vivas (residuales o con el registro dañado: no son ficheros vivos)" : "") +
            (ShowingDeleted(s) is not null ? $" · {deleted.Count} borrados mostrados" : "") +
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
        // Solo un borrado con su registro: una entrada solo residual no tiene runs (o son de otro fichero).
        FsRecoverButton.IsEnabled = SelectedFsRow is { Deleted.CanRecover: true } && _copyCts is null;
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
            if (item.Items.OfType<TreeViewItem>().FirstOrDefault(i => i.Tag is ulong && (string)i.Header == name) is not { } next)
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

    private void FsRecover_Click(object sender, RoutedEventArgs e)
    {
        if (FsSource is not { } s || SelectedFsRow is not { Deleted: { CanRecover: true } deleted })
        {
            return;
        }
        // El cuadro se abre de forma asíncrona para no bloquear la pulsación (UI Automation).
        Dispatcher.BeginInvoke(async () =>
        {
            var dlg = new SaveFileDialog
            {
                Title = "Recuperar fichero borrado como",
                FileName = deleted.Entry.Name,
                Filter = "Todos los ficheros (*.*)|*.*",
                OverwritePrompt = true,
                AddExtension = false,
            };
            if (dlg.ShowDialog(this) != true)
            {
                return;
            }
            // El propio cuadro pidió confirmación si el destino existía.
            await RecoverFileAsync(s, deleted, dlg.FileName, overwrite: File.Exists(dlg.FileName));
        });
    }

    /// <summary>
    /// Recupera el borrado en <paramref name="path"/> con la política del motor (nunca sobre el origen ni un
    /// dispositivo, sin sustituir un fichero existente salvo confirmación, escritura atómica) e informa de los
    /// tramos posiblemente sobrescritos.
    /// </summary>
    internal async Task<NtfsRecoveryReport?> RecoverFileAsync(SourceEntry s, NtfsDeletedEntry deleted, string path, bool overwrite)
    {
        if (_copyCts is not null)
        {
            return null;
        }
        _copyCts = new CancellationTokenSource();
        FsRecoverButton.IsEnabled = false;
        FsSaveAsButton.IsEnabled = false;
        var name = deleted.Entry.Name;
        try
        {
            using var task = s.Ntfs!.StartRecover(deleted.Entry.Record, deleted.Entry.Sequence, path, overwrite: overwrite);
            var progress = new Progress<TaskProgress>(p => FsStatusText.Text = $"Recuperando «{name}»… {p.Done:N0} de {p.Total:N0}");
            var state = await task.WaitAsync(progress, _copyCts.Token);
            if (state == EngineTaskState.Cancelled)
            {
                FsStatusText.Text = "Recuperación cancelada: no se escribió ningún fichero.";
                return null;
            }
            var r = task.GetReport();
            Log($"Borrado {name} (registro {deleted.Entry.Record}, secuencia {deleted.Entry.Sequence}) recuperado en {path}: " +
                $"{r.Bytes:N0} bytes, {r.UntrustedBytes:N0} no fiables ({r.PossiblyOverwrittenBytes:N0} posiblemente sobrescritos); " +
                $"recuperabilidad {r.Recoverability.Label()}{(r.Summary.Length > 0 ? ": " + r.Summary : "")}.");
            foreach (var x in r.Suspect)
            {
                Log($"  Posiblemente sobrescritos: bytes {x.Offset:N0}..{x.Offset + x.Length:N0} (LCN {x.Lcn:N0}, {x.Clusters:N0} clústeres): {x.Description}.");
            }
            FsStatusText.Text = r.AllTrustworthy
                ? $"Recuperado «{name}» en {path} ({r.Bytes:N0} bytes)."
                : $"Recuperado en {path} con {r.UntrustedBytes:N0} de {r.Bytes:N0} bytes NO FIABLES ({r.PossiblyOverwrittenBytes:N0} posiblemente sobrescritos).";
            if (!r.AllTrustworthy)
            {
                var ranges = string.Join("\n", r.Suspect.Take(10).Select(x =>
                    $"· bytes {x.Offset:N0}..{x.Offset + x.Length:N0}: {x.Description}"));
                MessageBox.Show(this,
                    $"{r.UntrustedBytes:N0} de {r.Bytes:N0} bytes no son datos fiables del fichero: " +
                    $"{r.PossiblyOverwrittenBytes:N0} se leyeron de clústeres que pueden haberse reutilizado (se copian tal como están) y " +
                    $"{r.FilledBytes:N0} se escribieron con el patrón de relleno (ilegibles o sin mapa)." +
                    (ranges.Length > 0 ? "\n\nTramos posiblemente sobrescritos:\n" + ranges + (r.Suspect.Count > 10 ? "\n· … (ver el registro de actividad)" : "") : ""),
                    "Recuperación con tramos no fiables", MessageBoxButton.OK, MessageBoxImage.Warning);
            }
            return r;
        }
        catch (EngineException ex)
        {
            FsStatusText.Text = $"No se pudo recuperar: {ex.Message}";
            ShowError("No se pudo recuperar el fichero", ex);
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
