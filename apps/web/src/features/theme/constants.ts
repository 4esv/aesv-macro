import { DEFAULT_THEMES } from './themes';
import { macroDarkTheme } from './themes/macro-dark';
import { macroLightTheme } from './themes/macro-light';

type DefaultTheme = (typeof DEFAULT_THEMES)[number]['id'];

export const DEFAULT_LIGHT_THEME: DefaultTheme = macroLightTheme.id;
export const DEFAULT_DARK_THEME: DefaultTheme = macroDarkTheme.id;

export { DEFAULT_THEMES };
