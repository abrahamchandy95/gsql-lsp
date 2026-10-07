local M = {}

--- The GitHub repository. PLACEHOLDER: the one place to change for the release
--- downloads (:GsqlInstall) and the nvim-treesitter registration.
M.repo_url = 'https://github.com/gsql-lsp/gsql-lsp'

--- Directory of this plugin (editors/neovim) and of the repository.
local plugin_dir = vim.fn.fnamemodify(debug.getinfo(1, 'S').source:sub(2), ':p:h:h:h')
local repo_dir = vim.fn.fnamemodify(plugin_dir, ':h:h')

M.plugin_dir = plugin_dir
M.grammar_dir = repo_dir .. '/tree-sitter-gsql'

local is_windows = vim.fn.has('win32') == 1

---@class gsql.Options
---@field lsp? boolean|vim.lsp.Config  Set to false to skip the language server, or a table to extend its config.
---@field cmd? string[]               Command that starts gsql-lsp (default: found on $PATH or installed by :GsqlInstall).
---@field parser? boolean             Set to false to not build the tree-sitter parser automatically.
---@field version? string             Release that :GsqlInstall downloads: 'latest' (default) or a tag such as 'v0.1.0'.
---@field install? false|'prompt'|'auto'  What to do when no server is found: nothing, ask, or install. Default: print a hint once.

---@type gsql.Options
M.config = { version = 'latest' }

local function notify(message, level)
  vim.notify('gsql: ' .. message, level or vim.log.levels.INFO)
end

--- Runs a command asynchronously; `on_exit` is called on the main loop with
--- the vim.SystemCompleted (code -1 when the program could not be started).
---@param cmd string[]
---@param opts? vim.SystemOpts
---@param on_exit fun(result: vim.SystemCompleted)
local function run(cmd, opts, on_exit)
  local ok, err = pcall(vim.system, cmd, vim.tbl_extend('force', { text = true }, opts or {}), function(result)
    vim.schedule(function()
      on_exit(result)
    end)
  end)
  if not ok then
    vim.schedule(function()
      on_exit({ code = -1, signal = 0, stdout = '', stderr = tostring(err) })
    end)
  end
end

--- The C compiler used for the parser: $CC, else cc, gcc or clang.
---@return string? command
function M.compiler()
  for _, cc in ipairs({ vim.env.CC or '', 'cc', 'gcc', 'clang' }) do
    if cc ~= '' and vim.fn.executable(cc) == 1 then
      return cc
    end
  end
end

local NO_COMPILER = 'no C compiler (cc, gcc or clang; or set $CC) found on PATH. Install one '
  .. '(macOS: `xcode-select --install`; Debian/Ubuntu: `apt install build-essential`; '
  .. 'Windows: install LLVM/clang or MinGW gcc), or run `:TSInstall gsql` with nvim-treesitter.'

--- Compiles tree-sitter-gsql into `parser/gsql.so` inside this plugin, so
--- Neovim can load it without nvim-treesitter.
---@param on_done? fun(ok: boolean)
function M.build_parser(on_done)
  local function finish(ok)
    if on_done then
      on_done(ok)
    end
  end
  local src = M.grammar_dir .. '/src'
  if not (vim.uv.fs_stat(src .. '/parser.c')) then
    notify(
      'cannot build the parser: ' .. src .. '/parser.c does not exist (the plugin was installed without the '
        .. 'tree-sitter-gsql folder). Install the whole repository, or run `:TSInstall gsql` with nvim-treesitter.',
      vim.log.levels.ERROR
    )
    return finish(false)
  end
  local cc = M.compiler()
  if not cc then
    notify('cannot build the parser: ' .. NO_COMPILER, vim.log.levels.ERROR)
    return finish(false)
  end
  local parser_dir = plugin_dir .. '/parser'
  vim.fn.mkdir(parser_dir, 'p')
  local cmd = { cc, '-o', parser_dir .. '/gsql.so', '-shared', '-fPIC', '-Os', '-std=c11', '-I', src, src .. '/parser.c' }
  run(cmd, nil, function(result)
    local ok = result.code == 0
    if ok then
      notify('built ' .. parser_dir .. '/gsql.so')
      -- Restart highlighting in open GSQL buffers.
      for _, buf in ipairs(vim.api.nvim_list_bufs()) do
        if vim.bo[buf].filetype == 'gsql' then
          pcall(vim.treesitter.start, buf, 'gsql')
        end
      end
    else
      notify('failed to build the parser with `' .. cc .. '`:\n' .. (result.stderr or ''), vim.log.levels.ERROR)
    end
    finish(ok)
  end)
end

--- Whether `parser/gsql.so` exists and is at least as new as the grammar.
local function parser_is_current()
  local uv = vim.uv or vim.loop
  local built = uv.fs_stat(plugin_dir .. '/parser/gsql.so')
  local source = uv.fs_stat(M.grammar_dir .. '/src/parser.c')
  if not built then
    return false
  end
  return not (source and source.mtime.sec > built.mtime.sec)
end

--- Builds the parser when it is missing or older than the grammar (needs a C
--- compiler and the repository's `tree-sitter-gsql/src`), so that syntax
--- highlighting works without any manual step. Silent when it cannot build
--- (`:checkhealth gsql` and the first-buffer hint say why).
---@param on_done? fun(ok: boolean)
function M.ensure_parser(on_done)
  if parser_is_current() then
    if on_done then
      on_done(true)
    end
    return
  end
  if not vim.uv.fs_stat(M.grammar_dir .. '/src/parser.c') or not M.compiler() then
    -- Installed without the grammar sources or without a compiler:
    -- highlighting then needs the parser from `:TSInstall gsql`.
    if on_done then
      on_done(false)
    end
    return
  end
  M.build_parser(on_done)
end

--- Registers the parser with nvim-treesitter (main or master branch), so that
--- `:TSInstall gsql` works.
function M.register_nvim_treesitter()
  local ok, parsers = pcall(require, 'nvim-treesitter.parsers')
  if not ok then
    return false
  end
  local install_info = {
    url = M.repo_url,
    location = 'tree-sitter-gsql',
    files = { 'src/parser.c' },
    queries = 'tree-sitter-gsql/queries',
  }
  if type(parsers.get_parser_configs) == 'function' then
    -- master branch
    parsers.get_parser_configs().gsql = { install_info = install_info, filetype = 'gsql' }
  else
    -- main branch: a plain table that is reloaded (and the TSUpdate event
    -- fired) on every update, so register now and again on each event.
    parsers.gsql = { install_info = install_info }
    vim.api.nvim_create_autocmd('User', {
      pattern = 'TSUpdate',
      callback = function()
        require('nvim-treesitter.parsers').gsql = { install_info = install_info }
      end,
    })
  end
  return true
end

--- Where `cargo install` and package managers put the server. A Neovim started
--- from Finder or a desktop launcher gets a minimal $PATH that has none of them.
local function fallback_dirs()
  local home = vim.uv.os_homedir() or ''
  return {
    home .. '/.local/bin',
    (vim.env.CARGO_HOME or (home .. '/.cargo')) .. '/bin',
    '/opt/homebrew/bin',
    '/usr/local/bin',
  }
end

local exe_name = 'gsql-lsp' .. (is_windows and '.exe' or '')

--- Where :GsqlInstall puts the server.
---@return string
function M.installed_path()
  return vim.fn.stdpath('data') .. '/gsql/bin/' .. exe_name
end

---@class gsql.Server
---@field cmd string[]
---@field found boolean
---@field origin? 'setup'|'path'|'installed'|'folder'  Where it was found.

--- Looks for the server: `setup({ cmd })`, $PATH, the copy that :GsqlInstall
--- made, then the usual install folders.
---@return gsql.Server
function M.resolve_server()
  if M.config.cmd then
    return { cmd = M.config.cmd, found = vim.fn.executable(M.config.cmd[1]) == 1, origin = 'setup' }
  end
  if vim.fn.executable('gsql-lsp') == 1 then
    return { cmd = { 'gsql-lsp' }, found = true, origin = 'path' }
  end
  if vim.fn.executable(M.installed_path()) == 1 then
    return { cmd = { M.installed_path() }, found = true, origin = 'installed' }
  end
  for _, dir in ipairs(fallback_dirs()) do
    local path = dir .. '/' .. exe_name
    if vim.fn.executable(path) == 1 then
      return { cmd = { path }, found = true, origin = 'folder' }
    end
  end
  return { cmd = { 'gsql-lsp' }, found = false }
end

--- The command that starts the server (see `resolve_server`).
---@return string[]
function M.server_command()
  return M.resolve_server().cmd
end

-- Installing the server from a release ---------------------------------------

--- Base URL of the releases, laid out like GitHub's: `<base>/latest/download/<file>`
--- and `<base>/download/<tag>/<file>`. Overridable with `vim.g.gsql_release_base_url`
--- or $GSQL_RELEASE_BASE_URL (a `file://` URL works).
---@return string
function M.release_base_url()
  local url = vim.g.gsql_release_base_url or vim.env.GSQL_RELEASE_BASE_URL
  if not url or url == '' then
    url = M.repo_url .. '/releases'
  end
  return (url:gsub('/+$', ''))
end

--- The release target (as named by .github/workflows/release.yml) and archive
--- type for a platform; nil and a reason when there is no build for it.
---@param sysname? string  vim.uv.os_uname().sysname
---@param machine? string  vim.uv.os_uname().machine
---@return { target: string, ext: 'tar.gz'|'zip' }? target
---@return string? err
function M.release_target(sysname, machine)
  local uname = vim.uv.os_uname()
  sysname = sysname or uname.sysname
  machine = (machine or uname.machine):lower()
  local arch
  if machine == 'x86_64' or machine == 'amd64' or machine == 'x64' then
    arch = 'x86_64'
  elseif machine == 'arm64' or machine == 'aarch64' then
    arch = 'aarch64'
  else
    return nil, ('no release for the CPU `%s`'):format(machine)
  end
  if sysname == 'Darwin' then
    return { target = arch .. '-apple-darwin', ext = 'tar.gz' }
  elseif sysname == 'Linux' then
    return { target = arch .. '-unknown-linux-musl', ext = 'tar.gz' }
  elseif sysname:find('^Windows') then
    if arch == 'x86_64' then
      return { target = 'x86_64-pc-windows-msvc', ext = 'zip' }
    end
    return nil, 'no release for Windows on ' .. arch
  end
  return nil, ('no release for the system `%s`'):format(sysname)
end

--- Finds the sha256 of `name` in the text of a `sha256sum` file.
---@param text string
---@param name string
---@return string?
function M.find_checksum(text, name)
  for line in vim.gsplit(text, '\n', { plain = true }) do
    local hash, file = line:match('^(%x+)%s+%*?(.-)%s*$')
    if hash and #hash == 64 and file == name then
      return hash:lower()
    end
  end
end

local installing = false

--- Downloads `url` to `dest` with curl (wget as a fallback).
---@param url string
---@param dest string
---@param on_done fun(err?: string)
local function download(url, dest, on_done)
  local cmd
  if vim.fn.executable('curl') == 1 then
    cmd = { 'curl', '--fail', '--silent', '--show-error', '--location', '--retry', '2', '--connect-timeout', '20', '-o', dest, url }
  elseif vim.fn.executable('wget') == 1 then
    cmd = { 'wget', '-q', '-O', dest, url }
  else
    return on_done('neither curl nor wget was found on PATH')
  end
  run(cmd, nil, function(result)
    if result.code ~= 0 then
      on_done(('download of %s failed: %s'):format(url, vim.trim(result.stderr or '')))
    else
      on_done()
    end
  end)
end

--- Unpacks `archive` into `dir` (tar; PowerShell for a zip on Windows).
---@param archive string
---@param dir string
---@param on_done fun(err?: string)
local function extract(archive, dir, on_done)
  local cmd
  local zip = archive:match('%.zip$') ~= nil
  if zip and is_windows and vim.fn.executable('powershell') == 1 then
    -- Single-quoted literals: `$`, backticks and `"` in a path are not interpreted.
    local function literal(path)
      return "'" .. path:gsub("'", "''") .. "'"
    end
    cmd = {
      'powershell', '-NoProfile', '-NonInteractive', '-Command',
      ('Expand-Archive -LiteralPath %s -DestinationPath %s -Force'):format(literal(archive), literal(dir)),
    }
  elseif vim.fn.executable('tar') == 1 then
    cmd = { 'tar', zip and '-xf' or '-xzf', archive, '-C', dir }
  elseif zip and vim.fn.executable('unzip') == 1 then
    cmd = { 'unzip', '-q', '-o', archive, '-d', dir }
  else
    return on_done('no tar (or unzip / PowerShell for a zip) found on PATH to unpack ' .. archive)
  end
  run(cmd, nil, function(result)
    on_done(result.code ~= 0 and ('unpacking %s failed: %s'):format(archive, vim.trim(result.stderr or '')) or nil)
  end)
end

--- Points the language server at a freshly installed binary and restarts it in
--- the open GSQL buffers (unless the user configured `cmd` themselves).
local function refresh_lsp()
  if M.config.cmd or not M._lsp_enabled then
    return
  end
  vim.lsp.config('gsql_lsp', { cmd = M.server_command() })
  for _, client in ipairs(vim.lsp.get_clients({ name = 'gsql_lsp' })) do
    client:stop()
  end
  vim.lsp.enable('gsql_lsp', false)
  vim.lsp.enable('gsql_lsp')
end

--- `install()` that waits for the result, for package manager hooks that end
--- when the hook returns (lazy.nvim's `build`): the download would otherwise be
--- cut short when Neovim exits right after, as in `nvim --headless '+Lazy! sync' +qa`.
---@param opts? { version?: string }
---@param timeout? integer  milliseconds (default 120000)
---@return boolean ok
function M.install_sync(opts, timeout)
  local done, ok = false, false
  M.install(opts, function(success)
    done, ok = true, success
  end)
  vim.wait(timeout or 120000, function()
    return done
  end, 50)
  return ok
end

--- Downloads the release archive for this platform, checks it against the
--- release's SHA256SUMS, and installs the server to `installed_path()`.
--- Asynchronous; progress and errors go through vim.notify.
---@param opts? { version?: string }
---@param on_done? fun(ok: boolean, err?: string)
function M.install(opts, on_done)
  opts = opts or {}
  if installing then
    notify('an installation is already running', vim.log.levels.WARN)
    return
  end
  installing = true
  local function finish(ok, err)
    installing = false
    if err then
      notify(err .. '\n(release location: ' .. M.release_base_url() .. '; see editors/neovim/README.md)', vim.log.levels.ERROR)
    end
    if on_done then
      on_done(ok, err)
    end
  end

  local platform, perr = M.release_target()
  if not platform then
    return finish(false, "can't install the server: " .. perr .. '. Build it with `cargo install --path crates/gsql-lsp`.')
  end
  local version = opts.version or M.config.version or 'latest'
  if version ~= 'latest' and not version:match('^v') then
    version = 'v' .. version
  end
  local base = M.release_base_url()
  local dir_url = version == 'latest' and (base .. '/latest/download') or (base .. '/download/' .. version)
  local name = 'gsql-lsp-' .. platform.target
  local archive_name = name .. '.' .. platform.ext

  local tmp = ('%s/gsql-install-%.0f'):format(vim.fn.stdpath('cache'), vim.uv.hrtime())
  vim.fn.mkdir(tmp .. '/out', 'p')
  local function fail(err)
    vim.fn.delete(tmp, 'rf')
    finish(false, err)
  end

  notify(('downloading %s (%s)'):format(archive_name, version))
  download(dir_url .. '/SHA256SUMS', tmp .. '/SHA256SUMS', function(err)
    if err then
      return fail(err)
    end
    local expected = M.find_checksum(table.concat(vim.fn.readfile(tmp .. '/SHA256SUMS'), '\n'), archive_name)
    if not expected then
      return fail(('%s/SHA256SUMS has no entry for %s'):format(dir_url, archive_name))
    end
    download(dir_url .. '/' .. archive_name, tmp .. '/' .. archive_name, function(err2)
      if err2 then
        return fail(err2)
      end
      local actual = vim.fn.sha256(vim.fn.readblob(tmp .. '/' .. archive_name))
      if actual ~= expected then
        return fail(('checksum mismatch for %s: expected %s, got %s. Nothing was installed.'):format(archive_name, expected, actual))
      end
      notify('checksum ok, unpacking')
      extract(tmp .. '/' .. archive_name, tmp .. '/out', function(err3)
        if err3 then
          return fail(err3)
        end
        local unpacked = tmp .. '/out/' .. name .. '/' .. exe_name
        if vim.fn.filereadable(unpacked) == 0 then
          unpacked = vim.fs.find(exe_name, { path = tmp .. '/out', type = 'file', limit = 1 })[1]
        end
        if not unpacked then
          return fail(archive_name .. ' does not contain ' .. exe_name)
        end
        vim.uv.fs_chmod(unpacked, tonumber('755', 8))
        run({ unpacked, '--version' }, nil, function(result)
          if result.code ~= 0 then
            return fail('the downloaded server does not run: ' .. vim.trim((result.stderr or '') .. (result.stdout or '')))
          end
          local dest = M.installed_path()
          vim.fn.mkdir(vim.fn.fnamemodify(dest, ':h'), 'p')
          -- Copy next to the destination, then rename over it, so that a running
          -- server keeps its old file and no half-written binary is ever used.
          local staged = dest .. '.new'
          local copied, cerr = vim.uv.fs_copyfile(unpacked, staged)
          if copied then
            vim.uv.fs_chmod(staged, tonumber('755', 8))
            copied, cerr = vim.uv.fs_rename(staged, dest)
          end
          if not copied then
            vim.uv.fs_unlink(staged)
            return fail(('cannot write %s: %s%s'):format(dest, cerr, is_windows and ' (stop the running server first)' or ''))
          end
          vim.fn.delete(tmp, 'rf')
          notify(('installed %s to %s'):format(vim.trim(result.stdout or ''), dest))
          refresh_lsp()
          finish(true)
        end)
      end)
    end)
  end)
end

-- Setup ----------------------------------------------------------------------

local checked = false

--- Runs once per session, when the first GSQL buffer appears: offers (or
--- performs) the server installation and explains a missing parser compiler.
local function first_buffer_checks()
  if checked then
    return
  end
  checked = true
  local install = M.config.install
  if not M.resolve_server().found and install ~= false then
    if install == 'auto' then
      M.install()
    elseif install == 'prompt' then
      vim.ui.select({ 'Install gsql-lsp', 'Not now' }, { prompt = 'gsql-lsp was not found. Download it from the GitHub release?' }, function(choice)
        if choice == 'Install gsql-lsp' then
          M.install()
        end
      end)
    else
      notify('the language server `gsql-lsp` was not found. Run :GsqlInstall to download it (see :checkhealth gsql).', vim.log.levels.WARN)
    end
  end
  if M.config.parser ~= false and not pcall(vim.treesitter.language.add, 'gsql') and not M.compiler() then
    notify('syntax highlighting needs the tree-sitter parser, and ' .. NO_COMPILER, vim.log.levels.WARN)
  end
end

---@param opts? gsql.Options
function M.setup(opts)
  opts = opts or {}
  M.config = vim.tbl_extend('force', { version = 'latest' }, opts)
  M._lsp_enabled = false
  if opts.lsp ~= false then
    local config = type(opts.lsp) == 'table' and vim.deepcopy(opts.lsp) or {}
    -- Only an explicitly configured command is kept fixed; otherwise it is
    -- searched again after :GsqlInstall.
    M.config.cmd = opts.cmd or config.cmd
    config.cmd = M.config.cmd or M.server_command()
    vim.lsp.config('gsql_lsp', config)
    vim.lsp.enable('gsql_lsp')
    M._lsp_enabled = true
  end
  M.register_nvim_treesitter()
  if opts.parser ~= false then
    M.ensure_parser()
  end

  -- Checks that need a GSQL buffer, so they do not run on every Neovim start.
  checked = false
  vim.api.nvim_create_autocmd('FileType', {
    group = vim.api.nvim_create_augroup('gsql_setup', { clear = true }),
    pattern = 'gsql',
    once = true,
    callback = function()
      vim.schedule(first_buffer_checks)
    end,
  })
  for _, buf in ipairs(vim.api.nvim_list_bufs()) do
    if vim.api.nvim_buf_is_loaded(buf) and vim.bo[buf].filetype == 'gsql' then
      vim.schedule(first_buffer_checks)
      break
    end
  end
end

return M
