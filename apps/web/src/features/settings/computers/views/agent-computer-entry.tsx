import { t } from '@app/lib/i18n';
import HardDrivesIcon from '@icon/system/regular/hard-drives.svg';
import { type JSX, Show } from 'solid-js';
import { SettingsSelect } from '../../components/settings-select';
import { WhereItRunsOption } from '../../components/where-it-runs-option';

/**
 * The agent editor's Where it runs option for the team's Macro computers: a
 * computer picker and the model. Shown only once the team has a computer.
 */
export function ComputerWhereItRuns(props: {
  computers: { id: string; name: string }[];
  harnessId: string;
  checked: boolean;
  modelPicker: JSX.Element;
  onSelect: (harnessId: string) => void;
}) {
  return (
    <Show when={props.computers.length > 0}>
      <WhereItRunsOption
        value="computer"
        title={t('computers.onComputer')}
        description={t('computers.onComputerHint')}
        icon={<HardDrivesIcon />}
        checked={props.checked}
        onSelect={() => {
          const first = props.computers[0];
          if (first) props.onSelect(first.id);
        }}
      >
        <div class="grid grid-cols-1 gap-3 @min-[440px]:grid-cols-[repeat(2,minmax(0,280px))]">
          <div class="flex min-w-0 flex-col gap-1.5">
            <span class="text-xs font-medium text-ink-muted">
              {t('computers.computerKind')}
            </span>
            <SettingsSelect
              label={t('computers.computerKind')}
              options={props.computers}
              value={props.harnessId}
              onChange={props.onSelect}
            />
          </div>
          {props.modelPicker}
        </div>
      </WhereItRunsOption>
    </Show>
  );
}
