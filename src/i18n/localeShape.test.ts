import { zhCN } from './zh-CN';
import { zhTW } from './zh-TW';
import { en } from './en';
import { ja } from './ja';
import { ko } from './ko';

function leafKeys(value: unknown, prefix = ''): string[] {
  if (Array.isArray(value) || typeof value !== 'object' || value === null) return [prefix];
  return Object.entries(value as Record<string, unknown>).flatMap(([key, child]) =>
    leafKeys(child, prefix ? `${prefix}.${key}` : key),
  );
}

const resources = { 'zh-CN': zhCN, 'zh-TW': zhTW, en, ja, ko } as const;
const expected = leafKeys(zhCN).sort();
for (const [locale, resource] of Object.entries(resources).filter(([locale]) => locale !== 'zh-CN')) {
  const actual = leafKeys(resource).sort();
  const missing = expected.filter(key => !actual.includes(key));
  const extra = actual.filter(key => !expected.includes(key));
  if (missing.length || extra.length) {
    throw new Error(`${locale} locale shape mismatch; missing=${missing.join(',')}; extra=${extra.join(',')}`);
  }
}

const requiredKeys = [
  'modal.sections.general',
  'modal.sections.privacy',
  'modal.sections.advanced',
  'modal.about.localFirst',
  'settings.permissions.checkFailed',
  'permissions.needsAttention',
] as const;

function translationAt(resource: unknown, key: string): unknown {
  return key.split('.').reduce<unknown>((value, part) => {
    if (!value || typeof value !== 'object' || Array.isArray(value)) return undefined;
    return (value as Record<string, unknown>)[part];
  }, resource);
}

for (const [locale, resource] of Object.entries(resources)) {
  const missing = requiredKeys.filter(key => typeof translationAt(resource, key) !== 'string');
  if (missing.length) {
    throw new Error(`${locale} missing active UI translations: ${missing.join(',')}`);
  }
}

console.log('localeShape.test.ts passed');
