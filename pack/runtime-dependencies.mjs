import fs from 'node:fs';
import path from 'node:path';

/** 按锁文件复制指定运行依赖及其依赖树，保留原目录层级与许可证。 */
export function copyRuntimeDependencies(source, destination, roots) {
  const root = path.resolve(source);
  const lock = JSON.parse(fs.readFileSync(path.join(root, 'package-lock.json'), 'utf8'));
  const copied = {};
  function locate(from, name) {
    let directory = from;
    while (directory === root || directory.startsWith(`${root}${path.sep}`)) {
      const candidate = path.join(directory, 'node_modules', name);
      if (fs.existsSync(path.join(candidate, 'package.json'))) return candidate;
      if (directory === root) break;
      directory = path.dirname(directory);
    }
    return undefined;
  }
  function copy(from, name, optional = false) {
    const directory = locate(from, name);
    if (!directory) {
      if (optional) return;
      throw new Error(`运行依赖缺失: ${name}`);
    }
    const relative = path.relative(root, directory).replaceAll('\\', '/');
    if (copied[relative]) return;
    const manifest = JSON.parse(fs.readFileSync(path.join(directory, 'package.json'), 'utf8'));
    const locked = lock.packages[relative];
    if (!locked?.integrity || locked.version !== manifest.version) throw new Error(`运行依赖与锁文件不符: ${relative}`);
    copied[relative] = { version: locked.version, integrity: locked.integrity };
    fs.cpSync(directory, path.join(destination, relative), {
      recursive: true,
      filter: (entry) => {
        if (fs.lstatSync(entry).isSymbolicLink()) throw new Error(`运行依赖包含链接: ${entry}`);
        if (entry === directory) return true;
        const segments = path.relative(directory, entry).split(path.sep);
        if (segments.some(segment => ['node_modules', 'test', 'tests', '__tests__', '__mocks__', 'coverage', '.github'].includes(segment))) return false;
        return !/\.(?:test|spec)\.[cm]?[jt]sx?(?:\.map)?$/.test(path.basename(entry));
      },
    });
    const optionalDependencies = manifest.optionalDependencies ?? {};
    for (const dependency of Object.keys(manifest.dependencies ?? {}))
      copy(directory, dependency, dependency in optionalDependencies);
    for (const dependency of Object.keys(optionalDependencies)) copy(directory, dependency, true);
    for (const dependency of Object.keys(manifest.peerDependencies ?? {}))
      copy(directory, dependency, manifest.peerDependenciesMeta?.[dependency]?.optional === true);
  }
  for (const name of roots) copy(root, name);
  return copied;
}
