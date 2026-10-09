import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import * as vscode from 'vscode';
import { LanguageClient, LanguageClientOptions, ServerOptions, TransportKind } from 'vscode-languageclient/node';

/** Where to get the server. The one place the repository URL appears in this file. */
const RELEASES_URL = 'https://github.com/abrahamchandy95/gsql-lsp/releases';

let client: LanguageClient | undefined;

/** The `gsql` settings as a plain object, sent as initialization options. */
function settings(): unknown {
  return JSON.parse(JSON.stringify(vscode.workspace.getConfiguration('gsql')));
}

/** `value` with a leading `~` and `${workspaceFolder}` (the first folder, when one is open) expanded. */
function expandPath(value: string): string {
  let result = value;
  if (result === '~' || result.startsWith('~/') || result.startsWith('~\\')) {
    result = os.homedir() + result.slice(1);
  }
  const folder = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
  return folder ? result.split('${workspaceFolder}').join(folder) : result;
}

/**
 * The server to run: an explicitly configured path, else the binary bundled in
 * platform-specific builds of the extension, else `gsql-lsp` from PATH.
 */
function serverCommand(context: vscode.ExtensionContext): string {
  const configured = vscode.workspace.getConfiguration('gsql').get<string>('server.path') || 'gsql-lsp';
  if (configured !== 'gsql-lsp') {
    return expandPath(configured);
  }
  const bundled = context.asAbsolutePath(path.join('server', process.platform === 'win32' ? 'gsql-lsp.exe' : 'gsql-lsp'));
  if (fs.existsSync(bundled)) {
    if (process.platform !== 'win32') {
      try {
        // Packaging does not always preserve the executable bit.
        fs.chmodSync(bundled, 0o755);
      } catch {
        // Fall through: starting the server reports a clear error if this matters.
      }
    }
    return bundled;
  }
  return configured;
}

/** Whether `command` is an existing file, or (for a bare name) found in a PATH directory. */
function resolvable(command: string): boolean {
  if (path.isAbsolute(command) || command.includes('/') || command.includes('\\')) {
    return fs.existsSync(command);
  }
  const extensions = process.platform === 'win32' ? (process.env.PATHEXT || '.EXE;.CMD;.BAT').split(';') : [''];
  return (process.env.PATH || '')
    .split(path.delimiter)
    .filter((dir) => dir.length > 0)
    .some((dir) => extensions.some((ext) => fs.existsSync(path.join(dir, command + ext))));
}

/** Tells the user how to get a server, with buttons for the setting and the releases page. */
async function reportMissingServer(command: string, detail: string): Promise<void> {
  const choice = await vscode.window.showErrorMessage(
    `Could not start gsql-lsp (${command}): ${detail}. Install a release build (or run \`cargo install --path crates/gsql-lsp\`) ` +
    'and make sure it is on PATH, or set "gsql.server.path" to the executable.',
    'Open Settings',
    'Open Releases',
  );
  if (choice === 'Open Settings') {
    await vscode.commands.executeCommand('workbench.action.openSettings', 'gsql.server.path');
  } else if (choice === 'Open Releases') {
    await vscode.env.openExternal(vscode.Uri.parse(RELEASES_URL));
  }
}

/** Stops the running client, if any; a client that failed to start cannot be stopped. */
async function stopClient(): Promise<void> {
  const running = client;
  client = undefined;
  try {
    await running?.stop();
  } catch {
    // Already stopped.
  }
}

async function startClient(context: vscode.ExtensionContext): Promise<void> {
  const command = serverCommand(context);
  if (!resolvable(command)) {
    await reportMissingServer(command, 'no such file or executable');
    return;
  }
  const serverOptions: ServerOptions = { command, args: [], transport: TransportKind.stdio };
  const clientOptions: LanguageClientOptions = {
    documentSelector: [
      { scheme: 'file', language: 'gsql' },
      { scheme: 'untitled', language: 'gsql' },
    ],
    // Sends `workspace/didChangeConfiguration` whenever a `gsql.*` setting changes.
    synchronize: { configurationSection: 'gsql' },
    initializationOptions: settings(),
  };
  client = new LanguageClient('gsql', 'GSQL Language Server', serverOptions, clientOptions);
  try {
    await client.start();
  } catch (error) {
    client = undefined;
    await reportMissingServer(command, String(error));
  }
}

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  context.subscriptions.push(
    vscode.commands.registerCommand('gsql.restartServer', async () => {
      await stopClient();
      await startClient(context);
    }),
    vscode.workspace.onDidChangeConfiguration(async (event) => {
      if (event.affectsConfiguration('gsql.server.path')) {
        await vscode.commands.executeCommand('gsql.restartServer');
      }
    }),
  );
  await startClient(context);
}

export async function deactivate(): Promise<void> {
  await stopClient();
}
