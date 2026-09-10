import type { AppTab } from '../state/useAppState';

/** 分组子项标题的 i18n key：nav.<id>。 */
export function subItemLabelKey(id: AppTab): string {
  return `nav.${id}`;
}
