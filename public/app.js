// State
let lastLogCount = 0;

async function updateState() {
    try {
        const res = await fetch('/api/state');
        if (!res.ok) throw new Error("Network response was not ok");
        const data = await res.json();

        // Header Status
        document.getElementById('botMode').textContent = data.mode;
        const stateEl = document.getElementById('botState');
        stateEl.textContent = data.state;
        stateEl.style.color = data.state === 'RUNNING' ? 'var(--accent-green)' : (data.state === 'PAUSED' ? 'var(--accent-yellow)' : 'var(--accent-red)');
        
        // Uptime
        const m = Math.floor(data.uptime_secs / 60);
        const s = data.uptime_secs % 60;
        document.getElementById('uptime').textContent = `${m}m ${s}s`;

        // KPIs
        const sessionPnl = document.getElementById('sessionPnl');
        const sign = data.session_pnl >= 0 ? '+' : '';
        sessionPnl.textContent = `${sign}$${data.session_pnl.toFixed(2)}`;
        sessionPnl.className = `kpi-value pnl-value ${data.session_pnl >= 0 ? 'positive' : 'negative'}`;

        document.getElementById('portfolioBalance').textContent = `$${data.current_balance.toFixed(2)}`;
        document.getElementById('openCount').textContent = data.positions.length;
        document.getElementById('modeLabel').textContent = data.mode;

        // Risk Bar
        document.getElementById('lossCount').textContent = data.losses;
        document.getElementById('lossLimit').textContent = data.halt_limit;
        const riskPct = data.halt_limit > 0 ? Math.min(100, (data.losses / data.halt_limit) * 100) : 0;
        const riskBar = document.getElementById('riskBar');
        riskBar.style.width = `${riskPct}%`;
        if (riskPct > 66) riskBar.style.backgroundColor = 'var(--accent-red)';
        else if (riskPct > 33) riskBar.style.backgroundColor = 'var(--accent-yellow)';
        else riskBar.style.backgroundColor = 'var(--accent-green)';

        // Positions Table
        const tbody = document.getElementById('positionsTable');
        tbody.innerHTML = '';
        data.positions.forEach(pos => {
            const tr = document.createElement('tr');
            
            let marketShort = pos.market_id.substring(0, 16) + (pos.market_id.length > 16 ? '…' : '');
            let sideClass = pos.side === 'Yes' ? 'side-yes' : 'side-no';
            
            let pnlStr = "—";
            let pnlClass = "text-neutral";
            if (pos.pnl !== null && pos.pnl !== undefined) {
                let p = parseFloat(pos.pnl);
                pnlStr = (p >= 0 ? '+' : '') + p.toFixed(2);
                pnlClass = p > 0 ? 'text-positive' : (p < 0 ? 'text-negative' : 'text-neutral');
            }

            // Convert to cents for entry
            let entryCents = Math.round(parseFloat(pos.entry_price) * 100);

            tr.innerHTML = `
                <td>${marketShort}</td>
                <td class="${sideClass}">${pos.side.toUpperCase()}</td>
                <td>$${parseFloat(pos.size).toFixed(2)}</td>
                <td>¢${entryCents}</td>
                <td class="${pnlClass} bold">${pnlStr}</td>
            `;
            tbody.appendChild(tr);
        });

        // Whales List
        const whalesList = document.getElementById('whalesList');
        whalesList.innerHTML = '';
        data.target_wallets.forEach(w => {
            let shortAddr = "0x" + w.address.substring(2, 8) + "…" + w.address.substring(38, 42);
            let wr = (w.win_rate * 100).toFixed(1);
            let wrColor = w.win_rate >= 0.55 ? 'var(--accent-green)' : 'var(--accent-yellow)';

            const el = document.createElement('div');
            el.className = 'whale-card';
            el.innerHTML = `
                <div class="whale-header">
                    <span class="whale-addr">${shortAddr}</span>
                    <span class="whale-trades">Trades: ${w.trade_count}</span>
                </div>
                <div class="whale-stats">
                    <span class="whale-wr" style="color: ${wrColor}">${wr}%</span>
                    <div class="wr-bar-bg">
                        <div class="wr-bar-fill" style="width: ${wr}%; background-color: ${wrColor}"></div>
                    </div>
                </div>
            `;
            whalesList.appendChild(el);
        });

    } catch (err) {
        console.error("State poll error:", err);
    }
}

async function updateLogs() {
    try {
        const res = await fetch('/api/logs');
        if (!res.ok) throw new Error("Network response was not ok");
        const logs = await res.json();

        const feedContainer = document.getElementById('feedContainer');
        
        // Only append new logs if count changed (basic check, could be better)
        if (logs.length > 0) {
            feedContainer.innerHTML = ''; // Full redraw for simplicity since array slices
            
            // Render latest at top
            [...logs].reverse().forEach(log => {
                const el = document.createElement('div');
                el.className = 'log-entry';
                el.innerHTML = `
                    <div class="log-time">${log.time}</div>
                    <div class="log-kind ${log.kind}">${log.kind}</div>
                    <div class="log-msg">${log.message}</div>
                `;
                feedContainer.appendChild(el);
            });
        }
    } catch (err) {
        console.error("Log poll error:", err);
    }
}

setInterval(updateState, 1000);
setInterval(updateLogs, 1000);
updateState();
updateLogs();
