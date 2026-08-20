import React, { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';

interface MicrophoneDevice {
  id: string;
  name: string;
  isDefault: boolean;
}

interface MicrophoneStatus {
  state: 'Stopped' | 'Starting' | 'Running' | 'Error';
  deviceId: string | null;
  deviceName: string | null;
  sampleRate: number | null;
  channels: number | null;
  destination: string | null;
  droppedSamples: number;
}

export function CrossPlatformMicControls({ destinationHost, destinationPort }: { destinationHost: string, destinationPort: number }) {
  const [devices, setDevices] = useState<MicrophoneDevice[]>([]);
  const [selectedDevice, setSelectedDevice] = useState<string>('default');
  const [status, setStatus] = useState<MicrophoneStatus | null>(null);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    refreshDevices();
    refreshStatus();
    const interval = setInterval(refreshStatus, 2000);
    return () => clearInterval(interval);
  }, []);

  const refreshDevices = async () => {
    try {
      const list = await invoke<MicrophoneDevice[]>('list_microphones');
      setDevices(list);
    } catch (e) {
      console.error('Failed to list microphones:', e);
    }
  };

  const refreshStatus = async () => {
    try {
      const s = await invoke<MicrophoneStatus>('microphone_status');
      setStatus(s);
    } catch (e) {
      console.error('Failed to get microphone status:', e);
    }
  };

  const handleStart = async () => {
    setLoading(true);
    try {
      await invoke('start_microphone', {
        deviceId: selectedDevice === 'default' ? null : selectedDevice,
        destinationHost,
        destinationPort
      });
      await refreshStatus();
    } catch (e) {
      console.error('Failed to start microphone:', e);
      alert(`Failed to start microphone: ${e}`);
    } finally {
      setLoading(false);
    }
  };

  const handleStop = async () => {
    setLoading(true);
    try {
      await invoke('stop_microphone');
      await refreshStatus();
    } catch (e) {
      console.error('Failed to stop microphone:', e);
    } finally {
      setLoading(false);
    }
  };

  const isRunning = status?.state === 'Running' || status?.state === 'Starting';

  return (
    <div className="p-4 bg-gray-900 rounded-lg border border-gray-700 space-y-4">
      <h3 className="text-sm font-semibold text-gray-200">Global Microphone Sender</h3>
      
      <div>
        <label className="text-xs text-gray-400 block mb-1">Device</label>
        <select 
          className="w-full bg-gray-800 border border-gray-600 rounded px-3 py-1.5 text-sm text-gray-200"
          value={selectedDevice}
          onChange={e => setSelectedDevice(e.target.value)}
          disabled={loading || isRunning}
        >
          <option value="default">System Default</option>
          {devices.map(d => (
            <option key={d.id} value={d.id}>{d.name}</option>
          ))}
        </select>
      </div>

      <div className="text-xs text-gray-400 space-y-1">
        <p>Status: <span className="font-medium text-gray-200">{status?.state || 'Stopped'}</span></p>
        {status?.destination && <p>Destination: {status.destination}</p>}
        {status?.sampleRate && <p>Format: {status.sampleRate}Hz, {status.channels}ch</p>}
        {isRunning && <p>Dropped Samples: {status?.droppedSamples || 0}</p>}
      </div>

      {isRunning ? (
        <button
          onClick={handleStop}
          disabled={loading}
          className="w-full py-2 rounded font-medium bg-red-600 hover:bg-red-700 text-white disabled:opacity-50"
        >
          {loading ? 'Working...' : 'Disable Microphone'}
        </button>
      ) : (
        <button
          onClick={handleStart}
          disabled={loading}
          className="w-full py-2 rounded font-medium bg-blue-600 hover:bg-blue-700 text-white disabled:opacity-50"
        >
          {loading ? 'Working...' : 'Enable Microphone'}
        </button>
      )}
    </div>
  );
}
