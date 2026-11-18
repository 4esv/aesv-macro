import { describe, expect, it } from 'vitest';
import { allOf } from './verdict';

describe('allOf', () => {
  it('is false once any part is false, even while others load', () => {
    expect(allOf(true, undefined, false)).toBe(false);
  });

  it('waits on any unknown part, then is true', () => {
    expect(allOf(true, undefined)).toBeUndefined();
    expect(allOf(true, true)).toBe(true);
    expect(allOf()).toBe(true);
  });
});
