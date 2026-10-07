-- Tests :GsqlInstall against a fake release on disk (no network): a tar.gz with
-- the locally built server plus a SHA256SUMS file, served through file:// URLs.
--
--   GSQL_LSP_BIN=target/debug/gsql-lsp nvim --headless --clean -u NONE -l editors/neovim/test/install.lua
--
-- Unix only (the fake archive is a tar.gz).

local plugin_dir = vim.fn.fnamemodify(debug.getinfo(1, 'S').source:sub(2), ':p:h:h')
vim.opt.rtp:prepend(plugin_dir)
vim.cmd('runtime! ftdetect/gsql.lua')
vim.cmd('runtime! plugin/gsql.lua')

local failures = 0
local function check(condition, message)
  if condition then
    print('ok   ' .. message)
  else
    failures = failures + 1
    print('FAIL ' .. message)
  end
end

local function finish()
  print(failures == 0 and 'all checks passed' or (failures .. ' check(s) failed'))
  vim.cmd(failures == 0 and 'qall!' or 'cquit!')
end

if vim.fn.has('win32') == 1 then
  print('skipped on Windows')
  return finish()
end

-- An empty home, data and cache directory, and a PATH without gsql-lsp.
local tmp = vim.fn.tempname()
vim.fn.mkdir(tmp, 'p')
vim.env.HOME = tmp .. '/home'
vim.env.XDG_DATA_HOME = tmp .. '/data'
vim.env.XDG_CACHE_HOME = tmp .. '/cache'
vim.env.CARGO_HOME = nil
vim.env.GSQL_RELEASE_BASE_URL = nil
vim.env.PATH = '/usr/bin:/bin'
vim.fn.mkdir(vim.env.HOME, 'p')

local gsql = require('gsql')
local bin = vim.fn.fnamemodify(vim.env.GSQL_LSP_BIN or 'target/debug/gsql-lsp', ':p')
check(vim.fn.executable(bin) == 1, 'the server binary to package exists: ' .. bin)
check(vim.startswith(gsql.installed_path(), tmp .. '/data/'), 'installs below stdpath("data"): ' .. gsql.installed_path())

-- Platform mapping, as named by .github/workflows/release.yml.
local function target(sys, machine)
  local t = gsql.release_target(sys, machine)
  return t and (t.target .. '.' .. t.ext)
end
check(target('Darwin', 'arm64') == 'aarch64-apple-darwin.tar.gz', 'macOS arm64 target')
check(target('Darwin', 'x86_64') == 'x86_64-apple-darwin.tar.gz', 'macOS x86_64 target')
check(target('Linux', 'x86_64') == 'x86_64-unknown-linux-musl.tar.gz', 'Linux x86_64 target')
check(target('Linux', 'aarch64') == 'aarch64-unknown-linux-musl.tar.gz', 'Linux aarch64 target')
check(target('Windows_NT', 'AMD64') == 'x86_64-pc-windows-msvc.zip', 'Windows x86_64 target')
check(target('Windows_NT', 'ARM64') == nil, 'Windows arm64 has no release')
check(target('FreeBSD', 'x86_64') == nil, 'FreeBSD has no release')
check(
  gsql.find_checksum(('%s  other\n%s *gsql-lsp-x.zip\n'):format(('a'):rep(64), ('B'):rep(64)), 'gsql-lsp-x.zip') == ('b'):rep(64),
  'find_checksum reads sha256sum lines (binary marker, case)'
)

-- The fake release: <base>/latest/download/* and <base>/download/<tag>/*.
local host = assert(gsql.release_target())
local name = 'gsql-lsp-' .. host.target
local archive = name .. '.tar.gz'
local function sh(cmd)
  local out = vim.fn.system({ 'sh', '-c', cmd })
  assert(vim.v.shell_error == 0, cmd .. ': ' .. out)
  return vim.trim(out)
end
local base = tmp .. '/releases'
local function release(dir, sums_hash)
  local stage = tmp .. '/stage/' .. name
  vim.fn.mkdir(stage, 'p')
  vim.fn.mkdir(dir, 'p')
  sh(('cp %s %s/gsql-lsp && echo readme > %s/README.md'):format(vim.fn.shellescape(bin), stage, stage))
  sh(('tar czf %s/%s -C %s %s'):format(dir, archive, tmp .. '/stage', name))
  local hash = sums_hash or sh(('shasum -a 256 %s/%s'):format(dir, archive)):match('^(%x+)')
  vim.fn.writefile({ hash .. '  ' .. archive, ('0'):rep(64) .. '  something-else.zip' }, dir .. '/SHA256SUMS')
end
release(base .. '/latest/download')
release(base .. '/download/v0.1.0')
release(base .. '/download/v9.9.9', ('0'):rep(64)) -- wrong checksum
vim.fn.mkdir(base .. '/download/v8.8.8', 'p')
vim.fn.writefile({ ('1'):rep(64) .. '  other.zip' }, base .. '/download/v8.8.8/SHA256SUMS') -- no entry

check(gsql.release_base_url() == gsql.repo_url .. '/releases', 'default release URL derives from the one repo constant')
vim.env.GSQL_RELEASE_BASE_URL = 'file://' .. base .. '/'
check(gsql.release_base_url() == 'file://' .. base, 'GSQL_RELEASE_BASE_URL overrides the release URL')
vim.g.gsql_release_base_url = 'file://' .. base
check(gsql.release_base_url() == 'file://' .. base, 'vim.g.gsql_release_base_url overrides the release URL')

local messages = {}
vim.notify = function(message, level)
  table.insert(messages, { message = message, level = level })
end
local function errors()
  return table.concat(
    vim.tbl_map(function(m)
      return m.level == vim.log.levels.ERROR and m.message or ''
    end, messages),
    '\n'
  )
end
local function install(opts)
  local done, ok, err
  gsql.install(opts, function(o, e)
    done, ok, err = true, o, e
  end)
  vim.wait(30000, function()
    return done
  end)
  return ok, err
end

-- Nothing installed yet; a GSQL buffer is open with no server to start.
local server = gsql.resolve_server()
check(not server.found and server.origin == nil, 'no server is found before the installation')
local root = tmp .. '/project'
vim.fn.mkdir(root .. '/.git', 'p')
vim.fn.writefile({ 'CREATE QUERY q() FOR GRAPH g {', '  PRINT 1;', '}' }, root .. '/q.gsql')
gsql.setup({ parser = false })
vim.cmd('edit ' .. root .. '/q.gsql')
vim.wait(200)
check(#vim.lsp.get_clients({ name = 'gsql_lsp' }) == 0, 'no client while the server is missing')
vim.wait(1000, function()
  return vim.iter(messages):any(function(m)
    return m.message:find(':GsqlInstall', 1, true) ~= nil
  end)
end)
check(
  vim.iter(messages):any(function(m)
    return m.message:find('was not found', 1, true) and m.message:find(':GsqlInstall', 1, true)
  end),
  'opening a GSQL file prints the install hint once'
)

-- Rejected archives.
local ok, err = install({ version = 'v9.9.9' })
check(not ok and err:find('checksum mismatch', 1, true), 'a checksum mismatch is rejected: ' .. tostring(err))
check(vim.fn.filereadable(gsql.installed_path()) == 0, 'nothing is installed after a checksum mismatch')
ok, err = install({ version = 'v8.8.8' })
check(not ok and err:find('no entry for ' .. archive, 1, true), 'a missing checksum entry is rejected: ' .. tostring(err))
ok, err = install({ version = 'v7.7.7' })
check(not ok and err:find('download of', 1, true) and err:find('v7.7.7', 1, true), 'a missing release is reported: ' .. tostring(err))
check(vim.fn.filereadable(gsql.installed_path()) == 0, 'nothing is installed after the failures')
check(#vim.fn.glob(tmp .. '/cache/nvim/gsql-install-*', false, true) == 0, 'temporary download folders are removed')

-- A good release, through the user command.
vim.cmd('GsqlInstall')
vim.wait(30000, function()
  return vim.fn.filereadable(gsql.installed_path()) == 1
end)
local installed = gsql.installed_path()
check(vim.fn.executable(installed) == 1, ':GsqlInstall installs an executable binary')
check(vim.uv.fs_stat(installed).mode % 512 == tonumber('755', 8), 'the binary has mode 0755')
local version = vim.system({ installed, '--version' }, { text = true }):wait()
check(version.code == 0 and vim.startswith(version.stdout, 'gsql-lsp'), 'the installed binary runs: ' .. vim.trim(version.stdout or ''))
server = gsql.resolve_server()
check(server.found and server.origin == 'installed' and server.cmd[1] == installed, 'server_command() finds the installed binary')
check(gsql.server_command()[1] == installed, 'server_command() returns it')
vim.wait(10000, function()
  return #vim.lsp.get_clients({ name = 'gsql_lsp' }) > 0
end)
local client = vim.lsp.get_clients({ name = 'gsql_lsp' })[1]
check(client and client.config.cmd[1] == installed, 'the open GSQL buffer gets a client running the installed binary')

-- The blocking variant (package manager hooks): it returns after the install, so a
-- Neovim that exits right away has a finished installation.
vim.fn.delete(gsql.installed_path())
check(gsql.install_sync() == true, 'install_sync() reports success')
check(vim.fn.executable(gsql.installed_path()) == 1, 'install_sync() has installed the binary when it returns')

-- Pinned version and update over an existing installation.
ok, err = install({ version = '0.1.0' })
check(ok, 'a pinned version (without the v) installs: ' .. tostring(err))
ok, err = install()
check(ok, 'installing again (update) replaces the binary: ' .. tostring(err))
check(vim.fn.filereadable(installed .. '.new') == 0, 'no staging file is left behind')

-- Priority: setup cmd > PATH > installed > standard folders.
local fake_dir = tmp .. '/pathbin'
vim.fn.mkdir(fake_dir, 'p')
sh(('cp %s %s/gsql-lsp'):format(vim.fn.shellescape(installed), fake_dir))
vim.env.PATH = fake_dir .. ':/usr/bin:/bin'
check(gsql.resolve_server().origin == 'path', 'a binary on PATH wins over the installed one')
gsql.config.cmd = { '/custom/gsql-lsp' }
check(gsql.resolve_server().origin == 'setup' and gsql.server_command()[1] == '/custom/gsql-lsp', 'setup({ cmd }) wins over PATH')
gsql.config.cmd = nil
vim.env.PATH = '/usr/bin:/bin'
vim.fn.delete(installed)
vim.fn.mkdir(vim.env.HOME .. '/.local/bin', 'p')
sh(('cp %s %s/.local/bin/gsql-lsp'):format(vim.fn.shellescape(bin), vim.env.HOME))
check(gsql.resolve_server().origin == 'folder', 'a standard folder is used when nothing else is found')


-- setup({ install = 'auto' }) downloads the server when the first GSQL buffer opens.
vim.fn.delete(installed)
vim.fn.delete(vim.env.HOME .. '/.local/bin/gsql-lsp')
check(not gsql.resolve_server().found, 'the server is gone again')
gsql.setup({ parser = false, install = 'auto' })
vim.cmd('enew | setfiletype gsql')
vim.wait(30000, function()
  return vim.fn.executable(installed) == 1
end)
check(vim.fn.executable(installed) == 1, "install = 'auto' installs the server when a GSQL buffer opens")

-- :checkhealth reports the origin.
vim.fn.delete(installed)
sh(('cp %s %s'):format(vim.fn.shellescape(bin), vim.fn.shellescape(installed)))
vim.cmd('checkhealth gsql')
local report = table.concat(vim.api.nvim_buf_get_lines(0, 0, -1, false), '\n')
check(report:find('installed by :GsqlInstall', 1, true) ~= nil, ':checkhealth gsql says where the binary came from')
check(report:find(':GsqlInstall downloads gsql-lsp-' .. host.target, 1, true) ~= nil, ':checkhealth gsql shows the download source')

vim.fn.delete(tmp, 'rf')
finish()
