import './style.scss';

import { useQuery, useQueryClient } from '@tanstack/react-query';
import { isUndefined } from 'lodash-es';
import { useEffect } from 'react';
import { useNavigate } from 'react-router';
import useBreakpoint from 'use-breakpoint';

import { Action } from '../../shared/components/layout/Action/Action';
import Button, {
  ButtonStyleVariant,
} from '../../shared/components/layout/Button/Button';
import PageContainer from '../../shared/components/layout/PageContainer/PageContainer';
import { IconEditNetwork } from '../../shared/components/svg';
import { deviceBreakpoints } from '../../shared/constants';
import { useModalStore } from '../../shared/hooks/store/useModalStore';
import useApi from '../../shared/hooks/useApi';
import { QueryKeys } from '../../shared/queries';
import { OverviewLayoutType } from '../../shared/types';
import { useNetworkPageStore } from '../network/hooks/useNetworkPageStore';
import GatewaySetupModal from '../vpn/modals/GatewaySetupModal/GatewaySetupModal';
import { getNetworkStatsFilterValue } from './helpers/stats';
import { useOverviewStore } from './hooks/store/useOverviewStore';
import { OverviewActivityStream } from './OverviewActivityStream/OverviewActivityStream';
import { OverviewConnectedUsers } from './OverviewConnectedUsers/OverviewConnectedUsers';
import { OverviewStats } from './OverviewStats/OverviewStats';
import { OverviewStatsFilterSelect } from './OverviewStatsFilterSelect/OverviewStatsFilterSelect';
import { OverviewViewSelect } from './OverviewViewSelect/OverviewViewSelect';

const STATUS_REFETCH_TIMEOUT = 15 * 1000;

export const OverviewPage = () => {
  const navigate = useNavigate();
  const { breakpoint } = useBreakpoint(deviceBreakpoints);
  const viewMode = useOverviewStore((state) => state.viewMode);
  const setOverViewStore = useOverviewStore((state) => state.setState);
  const statsFilter = useOverviewStore((state) => state.statsFilter);
  const setNetworkPageStore = useNetworkPageStore((state) => state.setState);
  const queryClient = useQueryClient();

  const setGatewaySetupModal = useModalStore(
    (state) => state.setGatewaySetupModal
  );
  const {
    network: { getNetworks, getUsersStats, getNetworkStats },
  } = useApi();

  const { data: networkStats } = useQuery(
    [QueryKeys.FETCH_NETWORK_STATS, statsFilter],
    () => getNetworkStats({ from: getNetworkStatsFilterValue(statsFilter) }),
    {
      onSuccess: async () => {
        setTimeout(
          () => queryClient.invalidateQueries([QueryKeys.FETCH_NETWORK_STATS]),
          STATUS_REFETCH_TIMEOUT
        );
      },
      refetchOnWindowFocus: false,
    }
  );

  const { data: networkUsersStats } = useQuery(
    [QueryKeys.FETCH_NETWORK_USERS_STATS, statsFilter],
    () => getUsersStats({ from: getNetworkStatsFilterValue(statsFilter) }),
    {
      enabled: !isUndefined(statsFilter),
      onSuccess: async () => {
        setTimeout(
          () =>
            queryClient.invalidateQueries([
              QueryKeys.FETCH_NETWORK_USERS_STATS,
            ]),
          STATUS_REFETCH_TIMEOUT
        );
      },
      refetchOnWindowFocus: false,
    }
  );

  const { data: networks, isLoading: networksLoading } = useQuery(
    [QueryKeys.FETCH_NETWORKS],
    getNetworks
  );

  useEffect(() => {
    if (breakpoint === 'mobile' && viewMode === OverviewLayoutType.LIST) {
      setOverViewStore({ viewMode: OverviewLayoutType.GRID });
    }
    if (breakpoint === 'tablet' && viewMode === OverviewLayoutType.GRID) {
      setOverViewStore({ viewMode: OverviewLayoutType.LIST });
    }
  }, [viewMode, breakpoint, setOverViewStore]);

  const handleNetworkAction = () => {
    if (networks && networks.length) {
      setNetworkPageStore({ network: networks[0] });
    }
    navigate('../network');
  };

  return (
    <>
      <PageContainer id="network-overview-page">
        {breakpoint !== 'desktop' && (
          <div className="mobile-options">
            <Button
              styleVariant={ButtonStyleVariant.STANDARD}
              text={
                isUndefined(networks) || !networks?.length
                  ? 'Configure network settings'
                  : 'Edit network settings'
              }
              icon={<IconEditNetwork />}
              disabled={networksLoading}
              onClick={handleNetworkAction}
            />
            <OverviewStatsFilterSelect />
            <OverviewViewSelect />
          </div>
        )}
        {breakpoint === 'desktop' && (
          <header>
            <h1>Network overview</h1>
            <OverviewViewSelect />
            <OverviewStatsFilterSelect />
            <Action
              onClick={() => setGatewaySetupModal({ visible: true })}
              className="docker-command"
            >
              Docker run command
            </Action>
            <Button
              styleVariant={ButtonStyleVariant.STANDARD}
              text={
                isUndefined(networks) || !networks?.length
                  ? 'Configure network settings'
                  : 'Edit network settings'
              }
              icon={<IconEditNetwork />}
              disabled={networksLoading}
              onClick={handleNetworkAction}
            />
          </header>
        )}
        {networkStats && networkUsersStats && (
          <OverviewStats
            usersStats={networkUsersStats}
            networkStats={networkStats}
          />
        )}
        <div className="bottom-row">
          <OverviewConnectedUsers stats={networkUsersStats} />
          <OverviewActivityStream />
        </div>
      </PageContainer>
      <GatewaySetupModal />
    </>
  );
};
