using System.Diagnostics;
using System.Globalization;
using System.IO;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using MemRescue.Core.Interop;
using Microsoft.Win32;

namespace MemRescue;

/// <summary>
/// Pestaña «Recuperación» (#26): árbol virtual del volumen con casillas, destino, opciones, progreso,
/// preguntas de conflicto, cancelación y resumen con el diario.
/// </summary>
public partial class MainWindow
{
    private CancellationTokenSource? _recoverCts;

    /// <summary>Nodo que hay que marcar al cargar el árbol (desde «Recuperar esta carpeta…»).</summary>
    private string? _recoverPendingPath;

    private void ShowRecoverTab_Click(object sender, RoutedEventArgs e)
    {
        MainTabs.SelectedItem = RecoverTab;
        UpdateRecoverView();
    }

    /// <summary>Estado de la pestaña según el origen seleccionado.</summary>
    private void UpdateRecoverView()
    {
        var s = Selected;
        var busy = _recoverCts is not null;
        RecoverLoadTreeButton.IsEnabled = s is not null && !busy;
        RecoverStartButton.IsEnabled = s?.Vfs is not null && !busy;
        FsRecoverFolderButton.IsEnabled = s?.Ntfs is not null && !busy;
        if (s is null)
        {
            RecoverVolumeText.Text = "Abra una imagen de disco y su sistema de archivos NTFS.";
            RecoverTree.Items.Clear();
            return;
        }
        RecoverVolumeText.Text = s.Ntfs is { } v
            ? $"Origen: {s.FileName} · NTFS «{v.Info.Label}». Marque los ficheros y carpetas que quiere recuperar."
            : $"Origen: {s.FileName}. «Cargar árbol» abre su sistema de archivos NTFS.";
        if (RecoverTree.Tag != s.Vfs)
        {
            FillRecoverTree(s);
        }
    }

    private async void RecoverLoadTree_Click(object sender, RoutedEventArgs e)
    {
        if (Selected is { } s)
        {
            await LoadRecoverTreeAsync(s);
        }
    }

    /// <summary>Abre el sistema de archivos si hace falta y construye el árbol virtual (una vez por origen).</summary>
    internal async Task<VirtualFileSystem?> LoadRecoverTreeAsync(SourceEntry s)
    {
        if (s.Vfs is null)
        {
            if (s.Ntfs is null && await OpenFileSystemAsync(s) is null)
            {
                return null;
            }
            MainTabs.SelectedItem = RecoverTab;
            RecoverLoadTreeButton.IsEnabled = false;
            try
            {
                using var task = s.Ntfs!.StartVirtualFileSystem();
                var progress = new Progress<TaskProgress>(p =>
                    RecoverTreeText.Text = $"Construyendo el árbol virtual… {p.Fraction:P0}");
                if (await task.WaitAsync(progress) != EngineTaskState.Completed)
                {
                    RecoverTreeText.Text = "Construcción del árbol cancelada.";
                    return null;
                }
                s.Vfs = task.GetResult();
                var i = s.Vfs.Info;
                Log($"Árbol virtual de {s.FileName}: {i.Files:N0} ficheros, {i.Directories:N0} directorios, {i.Deleted:N0} borrados.");
            }
            catch (EngineException ex)
            {
                ShowError("No se pudo construir el árbol virtual", ex);
                return null;
            }
        }
        if (Selected == s)
        {
            UpdateRecoverView();
            FillRecoverTree(s);
        }
        return s.Vfs;
    }

    private static string NodeLabel(VfsNode n, string name)
    {
        var state = n.Kind == VfsNodeKind.File ? $" · {n.DataState.Code()}" : "";
        var cat = n.Category == VfsCategory.Live ? "" : $" · {n.Category.Code()}";
        var size = n.Kind == VfsNodeKind.File ? $" · {n.Size.ToString("N0", CultureInfo.CurrentCulture)} B" : "";
        return name + cat + state + size;
    }

    private TreeViewItem RecoverItem(VirtualFileSystem vfs, VfsNode n)
    {
        var name = n.Id == VirtualFileSystem.Root ? "/" : vfs.DisplayNameOf(n.Id);
        var check = new CheckBox { Content = NodeLabel(n, name), Tag = n.Id };
        var id = n.Id.ToString(CultureInfo.InvariantCulture);
        AutomationProperties.SetAutomationId(check, "RecoverCheck" + id);
        AutomationProperties.SetName(check, $"{(n.Id == VirtualFileSystem.Root ? "/" : vfs.PathOf(n.Id))}: {NodeLabel(n, name)}");
        check.Checked += RecoverCheck_Changed;
        check.Unchecked += RecoverCheck_Changed;
        var item = new TreeViewItem { Header = check, Tag = n.Id };
        AutomationProperties.SetAutomationId(item, "RecoverNode" + id);
        AutomationProperties.SetName(item, name);
        if (n.IsContainer && n.ChildCount > 0)
        {
            item.Items.Add("…");
            item.Expanded += RecoverItem_Expanded;
        }
        return item;
    }

    private void FillRecoverTree(SourceEntry s)
    {
        RecoverTree.Items.Clear();
        RecoverTree.Tag = s.Vfs;
        if (s.Vfs is not { } vfs)
        {
            RecoverTreeText.Text = "";
            return;
        }
        var i = vfs.Info;
        RecoverTreeText.Text = $"{i.Files:N0} ficheros ({i.ByDataState[VfsDataState.Integro]:N0} íntegros), " +
            $"{i.Directories:N0} directorios, {i.Deleted:N0} borrados" + (i.Partial ? " · ÁRBOL INCOMPLETO" : "");
        var root = RecoverItem(vfs, vfs.GetNode(VirtualFileSystem.Root));
        RecoverTree.Items.Add(root);
        root.IsExpanded = true;
        if (_recoverPendingPath is { } p && vfs.Find(p) is { } target)
        {
            _recoverPendingPath = null;
            CheckRecoverPath(vfs, target.Id);
        }
    }

    private void RecoverItem_Expanded(object sender, RoutedEventArgs e)
    {
        if (e.OriginalSource is TreeViewItem item)
        {
            LoadRecoverChildren(item);
        }
    }

    private void LoadRecoverChildren(TreeViewItem item)
    {
        if (item.Items.Count != 1 || item.Items[0] is not string || RecoverTree.Tag is not VirtualFileSystem vfs
            || item.Tag is not uint id)
        {
            return;
        }
        item.Items.Clear();
        var parentChecked = (item.Header as CheckBox)?.IsChecked == true;
        foreach (var c in vfs.GetChildren(id))
        {
            var child = RecoverItem(vfs, c);
            if (parentChecked && child.Header is CheckBox cb)
            {
                cb.IsChecked = true;
            }
            item.Items.Add(child);
        }
    }

    /// <summary>Marca el nodo <paramref name="id"/> desplegando el árbol hasta él.</summary>
    private void CheckRecoverPath(VirtualFileSystem vfs, uint id)
    {
        var chain = new List<uint>();
        for (uint? cur = id; cur is { } c && c != VirtualFileSystem.Root; cur = vfs.GetNode(c).Parent)
        {
            chain.Add(c);
        }
        chain.Reverse();
        if (RecoverTree.Items.Count == 0 || RecoverTree.Items[0] is not TreeViewItem item)
        {
            return;
        }
        foreach (var c in chain)
        {
            LoadRecoverChildren(item);
            item.IsExpanded = true;
            if (item.Items.OfType<TreeViewItem>().FirstOrDefault(i => i.Tag is uint t && t == c) is not { } next)
            {
                return;
            }
            item = next;
        }
        if (item.Header is CheckBox cb)
        {
            cb.IsChecked = true;
        }
        item.IsSelected = true;
        item.BringIntoView();
    }

    /// <summary>Marcar o desmarcar un directorio hace lo mismo con los hijos ya cargados.</summary>
    private void RecoverCheck_Changed(object sender, RoutedEventArgs e)
    {
        if (sender is not CheckBox cb || cb.Parent is not TreeViewItem item)
        {
            return;
        }
        foreach (var child in item.Items.OfType<TreeViewItem>())
        {
            if (child.Header is CheckBox c && c.IsChecked != cb.IsChecked)
            {
                c.IsChecked = cb.IsChecked;
            }
        }
    }

    /// <summary>Nodos marcados (un nodo marcado incluye todo lo que cuelga de él).</summary>
    private List<uint> CheckedRecoverNodes()
    {
        var list = new List<uint>();
        void Walk(TreeViewItem item)
        {
            if (item.Header is CheckBox { IsChecked: true } && item.Tag is uint id)
            {
                list.Add(id);
                return;
            }
            foreach (var c in item.Items.OfType<TreeViewItem>())
            {
                Walk(c);
            }
        }
        foreach (var i in RecoverTree.Items.OfType<TreeViewItem>())
        {
            Walk(i);
        }
        return list;
    }

    private void RecoverBrowse_Click(object sender, RoutedEventArgs e)
    {
        // El cuadro se abre de forma asíncrona para no bloquear la pulsación (UI Automation).
        Dispatcher.BeginInvoke(() =>
        {
            var dlg = new OpenFolderDialog { Title = "Carpeta de destino de la recuperación" };
            if (dlg.ShowDialog(this) == true)
            {
                RecoverDestBox.Text = dlg.FolderName;
            }
        });
    }

    private void FsRecoverFolder_Click(object sender, RoutedEventArgs e)
    {
        if (FsSource is not { } s)
        {
            return;
        }
        _recoverPendingPath = string.IsNullOrWhiteSpace(FsPathBox.Text) ? "/" : FsPathBox.Text.Trim();
        MainTabs.SelectedItem = RecoverTab;
        if (s.Vfs is { } vfs)
        {
            UpdateRecoverView();
            if (vfs.Find(_recoverPendingPath) is { } n)
            {
                CheckRecoverPath(vfs, n.Id);
            }
            _recoverPendingPath = null;
        }
        else
        {
            _ = LoadRecoverTreeAsync(s);
        }
    }

    private RecoverConflictPolicy SelectedConflictPolicy => RecoverConflictBox.SelectedIndex switch
    {
        1 => RecoverConflictPolicy.Rename,
        2 => RecoverConflictPolicy.Skip,
        3 => RecoverConflictPolicy.SkipIfIdentical,
        4 => RecoverConflictPolicy.Overwrite,
        _ => RecoverConflictPolicy.Ask,
    };

    private void RecoverStart_Click(object sender, RoutedEventArgs e)
    {
        if (Selected is not { Vfs: not null } s)
        {
            return;
        }
        // Asíncrono para no bloquear la pulsación (UI Automation) con los cuadros de confirmación.
        Dispatcher.BeginInvoke(async () => await RecoverAsync(s));
    }

    private void RecoverCancel_Click(object sender, RoutedEventArgs e) => _recoverCts?.Cancel();

    /// <summary>Recupera los nodos marcados en el destino con las opciones de la pestaña.</summary>
    internal async Task<RecoverReport?> RecoverAsync(SourceEntry s)
    {
        if (_recoverCts is not null || s.Vfs is not { } vfs || s.Ntfs is not { } vol)
        {
            return null;
        }
        var nodes = CheckedRecoverNodes();
        if (nodes.Count == 0)
        {
            RecoverSummaryText.Text = "Marque al menos un fichero o una carpeta del árbol.";
            return null;
        }
        var dest = RecoverDestBox.Text.Trim();
        if (dest.Length == 0)
        {
            RecoverSummaryText.Text = "Indique la carpeta de destino.";
            return null;
        }
        var policy = SelectedConflictPolicy;
        var confirm = false;
        if (policy == RecoverConflictPolicy.Overwrite)
        {
            confirm = MessageBox.Show(this,
                "Los ficheros que ya existan en el destino con el mismo nombre se SUSTITUIRÁN (de forma atómica). " +
                "Los directorios existentes se reutilizan. ¿Continuar?",
                "Sustituir ficheros existentes", MessageBoxButton.YesNo, MessageBoxImage.Warning) == MessageBoxResult.Yes;
            if (!confirm)
            {
                return null;
            }
        }
        var cats = new List<VfsCategory>();
        if (RecoverCatLive.IsChecked == true)
        {
            cats.Add(VfsCategory.Live);
        }
        if (RecoverCatDeleted.IsChecked == true)
        {
            cats.Add(VfsCategory.Deleted);
        }
        if (RecoverCatOrphan.IsChecked == true)
        {
            cats.Add(VfsCategory.Orphan);
        }
        if (RecoverCatMetadata.IsChecked == true)
        {
            cats.Add(VfsCategory.Metadata);
        }
        if (cats.Count == 0)
        {
            RecoverSummaryText.Text = "Elija al menos una categoría.";
            return null;
        }
        var states = new List<VfsDataState> { VfsDataState.Integro, VfsDataState.IntegroNoVerificable, VfsDataState.Parcial };
        if (RecoverIncludeOverwritten.IsChecked == true)
        {
            states.Add(VfsDataState.Sobrescrito);
        }
        var options = new RecoverOptions
        {
            Conflict = policy,
            ConfirmOverwrite = confirm,
            Categories = cats,
            WriteStates = states,
            IncludeReallocatedBytes = RecoverIncludeReallocated.IsChecked == true,
            Masks = RecoverMasksBox.Text.Trim(),
        };
        _recoverCts = new CancellationTokenSource();
        UpdateRecoverView();
        RecoverCancelButton.IsEnabled = true;
        RecoverOpenJournalButton.IsEnabled = false;
        RecoverOpenDestinationButton.IsEnabled = false;
        RecoverJournalBox.Text = "";
        RecoverSummaryText.Text = "";
        RecoverProgressBar.Value = 0;
        try
        {
            using var task = vfs.StartRecover(vol, nodes, dest, options);
            Log($"Recuperación de {nodes.Count} elementos de {s.FileName} en {dest} (conflictos: {policy}).");
            var cancelRequested = false;
            while (task.Status().State == EngineTaskState.Running)
            {
                var p = task.Progress();
                RecoverProgressBar.Value = p.BytesTotal == 0 ? 0 : (double)p.BytesDone / p.BytesTotal;
                RecoverProgressText.Text = $"{p.FilesDone:N0} de {p.FilesTotal:N0} ficheros · {p.BytesDone:N0} de {p.BytesTotal:N0} bytes" +
                    (p.UntrustedBytes > 0 ? $" · {p.UntrustedBytes:N0} NO FIABLES" : "") +
                    (p.Errors > 0 ? $" · {p.Errors:N0} errores" : "");
                if (_recoverCts.IsCancellationRequested && !cancelRequested)
                {
                    task.Cancel();
                    cancelRequested = true;
                }
                if (p.QuestionPending && !cancelRequested && task.PendingQuestion() is { } q)
                {
                    var dlg = new RecoverConflictDialog(q) { Owner = this };
                    dlg.ShowDialog();
                    task.Answer(q.Id, dlg.Decision, dlg.ApplyToAll);
                    continue;
                }
                await Task.Delay(100);
            }
            var r = task.GetReport();
            RecoverProgressBar.Value = 1;
            RecoverProgressText.Text = $"{r.FilesWritten:N0} ficheros escritos de {r.FilesPlanned:N0}";
            RecoverSummaryText.Text = RecoverSummary(r);
            RecoverJournalBox.Text = r.JournalPath;
            RecoverOpenJournalButton.IsEnabled = File.Exists(r.JournalPath);
            RecoverOpenDestinationButton.IsEnabled = Directory.Exists(r.Destination);
            Log($"Recuperación terminada ({r.Status}): {RecoverSummary(r)} Diario: {r.JournalPath}");
            return r;
        }
        catch (EngineException ex)
        {
            RecoverSummaryText.Text = $"No se pudo recuperar: {ex.Message}";
            ShowError("No se pudo recuperar", ex);
            return null;
        }
        finally
        {
            _recoverCts.Dispose();
            _recoverCts = null;
            RecoverCancelButton.IsEnabled = false;
            UpdateRecoverView();
        }
    }

    private static string RecoverSummary(RecoverReport r)
    {
        var head = r.Status switch
        {
            RecoverStatus.Cancelled => "CANCELADA: solo se conservan los ficheros terminados. ",
            RecoverStatus.Failed => $"DETENIDA: {r.Reason}. ",
            _ => r.AllIntact ? "Completa: todo lo escrito es íntegro. " : "Completa, con ficheros que NO son íntegros (ver el diario). ",
        };
        return head +
            $"{r.FilesWritten:N0} ficheros escritos: {r.WrittenIntegro:N0} íntegros, " +
            $"{r.WrittenIntegroNoVerificable:N0} borrados sin errores (procedencia no verificable), " +
            $"{r.WrittenParcial:N0} parciales y {r.WrittenSobrescrito:N0} sobrescritos (con relleno; marcados con .mrbad). " +
            $"{r.SkippedState:N0} omitidos por su estado, {r.SkippedConflict:N0} por conflicto, {r.SkippedIdentical:N0} idénticos ya presentes, " +
            $"{r.Unsupported:N0} no soportados, {r.Errors:N0} errores. " +
            $"{r.DirectoriesCreated:N0} carpetas creadas; {r.UntrustedBytes:N0} bytes no fiables" +
            (r.LongPaths > 0 ? $"; {r.LongPaths:N0} rutas de más de 259 caracteres" : "") + ".";
    }

    private void RecoverOpenJournal_Click(object sender, RoutedEventArgs e)
    {
        if (File.Exists(RecoverJournalBox.Text))
        {
            Process.Start(new ProcessStartInfo("notepad.exe", $"\"{RecoverJournalBox.Text}\"") { UseShellExecute = false });
        }
    }

    private void RecoverOpenDestination_Click(object sender, RoutedEventArgs e)
    {
        var dir = Path.GetDirectoryName(Path.GetDirectoryName(RecoverJournalBox.Text));
        var dest = RecoverDestBox.Text.Trim();
        var target = Directory.Exists(dest) ? dest : dir;
        if (target is not null && Directory.Exists(target))
        {
            Process.Start(new ProcessStartInfo("explorer.exe", $"\"{target}\"") { UseShellExecute = false });
        }
    }
}
