import { cleanup, render, screen } from '@solidjs/testing-library';
import { afterEach, expect, it } from 'vitest';
import type { PublicProfile } from '../core/types';
import type { BookingSource } from '../primitives/booking-flow';
import { PublicBookingView } from './public-booking-view';

const profile: PublicProfile = {
  id: 'team',
  name: 'aesv',
  description: 'Meet the team.',
  eventTypes: [],
};

function renderPage(embedded?: boolean) {
  render(() => (
    <PublicBookingView
      profile={profile}
      source={{} as BookingSource}
      onEvent={() => {}}
      onReceipt={() => {}}
      embedded={embedded}
    />
  ));
}

afterEach(cleanup);

it('carries the aesv name when it is the whole page', () => {
  renderPage();

  expect(screen.getByText('/ Scheduling')).toBeTruthy();
  expect(screen.queryByRole('link')).toBeNull();
  expect(screen.getByText('Scheduling by aesv')).toBeTruthy();
});

it('leaves the aesv name to the page that embeds it', () => {
  renderPage(true);

  expect(screen.getByRole('heading', { name: 'aesv' })).toBeTruthy();
  expect(screen.queryByRole('link')).toBeNull();
  expect(screen.queryByText('Scheduling by aesv')).toBeNull();
});
