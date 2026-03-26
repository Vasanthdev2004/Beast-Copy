// ============================================================
// POLY-APEX DASHBOARD — Client
// ============================================================

(function () {
    'use strict';

    // ── State ──
    let connected = false;
    let lastLogSignature = '';   // fingerprint to detect new logs
    let renderedLogCount = 0;
    let userScrolledUp = false;  // manual scroll override
    let previousPositionKeys = new Set();

    // ── DOM refs (cached once) ──
    const $ = (id) => document.getElementById(id);
    const dom = {
        botMode:          $('botMode'),
        botState:         $('botState'),
        connDot:          $('connDot'),
        connLabel:        $('connLabel'),
        uptime:           $('uptime'),
        sessionPnl:       $('sessionPnl'),
        pnlRoi:           $('pnlRoi'),
        portfolioBalance: $('portfolioBalance'),
        openCount:        $('openCount'),
        maxPositions:     $('maxPositions'),
        winRate:          $('winRate'),
        dailyLoss:        $('dailyLoss'),
        dailyMaxLoss:     $('dailyMaxLoss'),
        dailyLossBar:     $('dailyLossBar'),
        totalTrades:      $('totalTrades'),
        modeLabel:        $('modeLabel'),
        positionsTable:   $('positionsTable'),
        positionsEmpty:   $('positionsEmpty'),
        whalesList:       $('whalesList'),
        feedContainer:    $('feedContainer'),
        lossCount:        $('lossCount'),
        lossLimit:        $('lossLimit'),
        lossGaugeFill:    $('lossGaugeFill'),
        dailyGaugeFill:   $('dailyGaugeFill'),
        footerDailyLoss:  $('footerDailyLoss'),
        footerDailyMax:   $('footerDailyMax'),
        haltTimer:        $('haltTimer'),
    };

    // ── Feed scroll tracking ──
    const feedEl = dom.feedContainer;
    feedEl.addEventListener('scroll', () => {
        const atBottom = feedEl.scrollHeight - feedEl.scrollTop - feedEl.clientHeight < 40;
        userScrolledUp = !atBottom;
    });

    // ── Connection status ──
    function setConnected(val) {
        if (connected === val) return;
        connected = val;
        dom.connDot.className = 'conn-dot ' + (val ? 'connected' : 'disconnected');
        dom.connLabel.textContent = val ? 'Connected' : 'Disconnected';
    }

    // ── Formatting helpers ──
    function fmtUsd(n) {
        const abs = Math.abs(n);
        return (n < 0 ? '-' : '') + '$' + abs.toFixed(2);
    }

    function fmtPnl(n) {
        const sign = n >= 0 ? '+' : '';
        return sign + '$' + Math.abs(n).toFixed(2);
    }

    function fmtUptime(secs) {
        const h = Math.floor(secs / 3600);
        const m = Math.floor((secs % 3600) / 60);
        const s = secs % 60;
        if (h > 0) return h + 'h ' + m + 'm ' + s + 's';
        return m + 'm ' + s + 's';
    }

    function fmtAge(openedAt) {
        if (!openedAt) return '--';
        const nowSecs = Math.floor(Date.now() / 1000);
        const diff = nowSecs - openedAt;
        if (diff < 0) return '--';
        if (diff < 60) return diff + 's';
        if (diff < 3600) return Math.floor(diff / 60) + 'm';
        if (diff < 86400) return Math.floor(diff / 3600) + 'h ' + Math.floor((diff % 3600) / 60) + 'm';
        return Math.floor(diff / 86400) + 'd';
    }

    function gaugeColor(pct) {
        if (pct > 75) return 'var(--accent-red)';
        if (pct > 40) return 'var(--accent-yellow)';
        return 'var(--accent-green)';
    }

    function truncate(str, max) {
        return str && str.length > max ? str.substring(0, max) + '\u2026' : (str || '');
    }

    // ── UPDATE STATE ──
    async function updateState() {
        try {
            const res = await fetch('/api/state');
            if (!res.ok) throw new Error('HTTP ' + res.status);
            const d = await res.json();
            setConnected(true);

            // Header
            dom.botMode.textContent = d.mode;
            dom.botMode.setAttribute('data-mode', d.mode);
            dom.botState.textContent = d.state;
            dom.botState.setAttribute('data-state', d.state);
            dom.uptime.textContent = fmtUptime(d.uptime_secs);

            // P&L
            dom.sessionPnl.textContent = fmtPnl(d.session_pnl);
            dom.sessionPnl.className = 'kpi-value mono pnl-value ' + (d.session_pnl >= 0 ? 'positive' : 'negative');
            const roi = d.initial_balance > 0 ? ((d.session_pnl / d.initial_balance) * 100) : 0;
            dom.pnlRoi.textContent = (roi >= 0 ? '+' : '') + roi.toFixed(2) + '%';
            dom.pnlRoi.style.color = roi >= 0 ? 'var(--accent-green)' : 'var(--accent-red)';

            // Balance
            dom.portfolioBalance.textContent = fmtUsd(d.current_balance);

            // Open positions / max
            dom.openCount.textContent = d.positions.length;
            dom.maxPositions.textContent = d.max_open_positions;
            dom.modeLabel.textContent = d.mode;

            // Win rate
            dom.winRate.textContent = (d.win_rate * 100).toFixed(1) + '%';

            // Daily loss
            const dlAbs = Math.abs(d.daily_loss);
            dom.dailyLoss.textContent = fmtUsd(dlAbs);
            dom.dailyMaxLoss.textContent = d.daily_max_loss.toFixed(0);
            const dlPct = d.daily_max_loss > 0 ? Math.min(100, (dlAbs / d.daily_max_loss) * 100) : 0;
            dom.dailyLossBar.style.width = dlPct + '%';
            dom.dailyLossBar.style.backgroundColor = gaugeColor(dlPct);

            // Total trades
            dom.totalTrades.textContent = d.total_trades_session;

            // ── Positions (incremental) ──
            updatePositions(d.positions);

            // ── Wallets (incremental) ──
            updateWallets(d.target_wallets);

            // ── Footer risk gauges ──
            dom.lossCount.textContent = d.losses;
            dom.lossLimit.textContent = d.halt_limit;
            const lossPct = d.halt_limit > 0 ? Math.min(100, (d.losses / d.halt_limit) * 100) : 0;
            dom.lossGaugeFill.style.width = lossPct + '%';
            dom.lossGaugeFill.style.backgroundColor = gaugeColor(lossPct);

            dom.footerDailyLoss.textContent = fmtUsd(dlAbs);
            dom.footerDailyMax.textContent = d.daily_max_loss.toFixed(0);
            dom.dailyGaugeFill.style.width = dlPct + '%';
            dom.dailyGaugeFill.style.backgroundColor = gaugeColor(dlPct);

            // Halt indicator
            dom.haltTimer.style.display = d.state === 'HALTED' ? 'flex' : 'none';

        } catch (err) {
            setConnected(false);
        }
    }

    // ── INCREMENTAL POSITIONS UPDATE ──
    function updatePositions(positions) {
        const tbody = dom.positionsTable;
        const currentKeys = new Set();

        if (positions.length === 0) {
            tbody.innerHTML = '';
            dom.positionsEmpty.style.display = 'block';
            previousPositionKeys.clear();
            return;
        }
        dom.positionsEmpty.style.display = 'none';

        // Build a map of existing rows by key
        const existingRows = {};
        for (const row of tbody.querySelectorAll('tr[data-key]')) {
            existingRows[row.getAttribute('data-key')] = row;
        }

        positions.forEach(pos => {
            const key = pos.market_id + '-' + pos.opened_at;
            currentKeys.add(key);

            const marketLabel = truncate(pos.market_name || pos.market_id, 22);
            const sideUpper = (typeof pos.side === 'string' ? pos.side : (pos.side === 'Yes' || pos.side?.Yes !== undefined ? 'Yes' : 'No')).toUpperCase();
            const sideClass = sideUpper === 'YES' ? 'yes' : 'no';
            const entryCents = Math.round(parseFloat(pos.entry_price) * 100);
            const age = fmtAge(pos.opened_at);

            let pnlStr = '\u2014';
            let pnlClass = 'text-neutral';
            if (pos.pnl !== null && pos.pnl !== undefined) {
                const p = parseFloat(pos.pnl);
                pnlStr = fmtPnl(p);
                pnlClass = p > 0 ? 'text-positive' : (p < 0 ? 'text-negative' : 'text-neutral');
            }

            if (existingRows[key]) {
                // Update existing row cells
                const cells = existingRows[key].cells;
                cells[0].textContent = marketLabel;
                cells[0].title = pos.market_id;
                cells[1].innerHTML = '<span class="side-badge ' + sideClass + '">' + sideUpper + '</span>';
                cells[2].textContent = '$' + parseFloat(pos.size).toFixed(2);
                cells[3].textContent = '\u00A2' + entryCents;
                cells[4].textContent = pnlStr;
                cells[4].className = pnlClass + ' bold';
                cells[5].textContent = age;
                delete existingRows[key];
            } else {
                // New row
                const tr = document.createElement('tr');
                tr.setAttribute('data-key', key);
                tr.innerHTML =
                    '<td title="' + (pos.market_id || '') + '">' + marketLabel + '</td>' +
                    '<td><span class="side-badge ' + sideClass + '">' + sideUpper + '</span></td>' +
                    '<td>$' + parseFloat(pos.size).toFixed(2) + '</td>' +
                    '<td>\u00A2' + entryCents + '</td>' +
                    '<td class="' + pnlClass + ' bold">' + pnlStr + '</td>' +
                    '<td class="age-cell">' + age + '</td>';
                tbody.appendChild(tr);
            }
        });

        // Remove stale rows
        for (const key in existingRows) {
            existingRows[key].remove();
        }

        previousPositionKeys = currentKeys;
    }

    // ── INCREMENTAL WALLETS UPDATE ──
    function updateWallets(wallets) {
        const container = dom.whalesList;

        if (wallets.length === 0) {
            if (!container.querySelector('.table-empty')) {
                container.innerHTML = '<div class="table-empty">No wallets tracked</div>';
            }
            return;
        }

        // Remove empty message if present
        const emptyMsg = container.querySelector('.table-empty');
        if (emptyMsg) emptyMsg.remove();

        // Build map of existing cards
        const existingCards = {};
        for (const card of container.querySelectorAll('.whale-card[data-addr]')) {
            existingCards[card.getAttribute('data-addr')] = card;
        }

        const seenAddrs = new Set();

        wallets.forEach(w => {
            const addr = w.address;
            seenAddrs.add(addr);
            const shortAddr = '0x' + addr.substring(2, 8) + '\u2026' + addr.substring(38, 42);
            const wr = (w.win_rate * 100).toFixed(1);
            const wrColor = w.win_rate >= 0.55 ? 'var(--accent-green)' : (w.win_rate >= 0.45 ? 'var(--accent-yellow)' : 'var(--accent-red)');
            const pnl = parseFloat(w.total_pnl || 0);
            const pnlStr = fmtPnl(pnl);
            const pnlColor = pnl >= 0 ? 'var(--accent-green)' : 'var(--accent-red)';

            if (existingCards[addr]) {
                // Update in place
                const card = existingCards[addr];
                card.querySelector('.whale-addr').textContent = shortAddr;
                const valEls = card.querySelectorAll('.whale-stat-val');
                if (valEls[0]) valEls[0].textContent = w.trade_count;
                const wrEl = card.querySelector('.whale-wr');
                if (wrEl) { wrEl.textContent = wr + '%'; wrEl.style.color = wrColor; }
                const fillEl = card.querySelector('.wr-bar-fill');
                if (fillEl) { fillEl.style.width = wr + '%'; fillEl.style.backgroundColor = wrColor; }
                const pnlEl = card.querySelector('.whale-pnl');
                if (pnlEl) { pnlEl.textContent = pnlStr; pnlEl.style.color = pnlColor; }
                delete existingCards[addr];
            } else {
                // New card
                const el = document.createElement('div');
                el.className = 'whale-card';
                el.setAttribute('data-addr', addr);
                el.innerHTML =
                    '<div class="whale-header">' +
                        '<span class="whale-addr">' + shortAddr + '</span>' +
                        '<span class="whale-meta">' +
                            '<span class="whale-stat">Trades: <span class="whale-stat-val">' + w.trade_count + '</span></span>' +
                        '</span>' +
                    '</div>' +
                    '<div class="whale-stats">' +
                        '<span class="whale-wr" style="color:' + wrColor + '">' + wr + '%</span>' +
                        '<div class="wr-bar-bg">' +
                            '<div class="wr-bar-fill" style="width:' + wr + '%;background-color:' + wrColor + '"></div>' +
                        '</div>' +
                        '<span class="whale-pnl" style="color:' + pnlColor + '">' + pnlStr + '</span>' +
                    '</div>';
                container.appendChild(el);
            }
        });

        // Remove stale cards
        for (const addr in existingCards) {
            existingCards[addr].remove();
        }
    }

    // ── UPDATE LOGS (incremental) ──
    async function updateLogs() {
        try {
            const res = await fetch('/api/logs');
            if (!res.ok) throw new Error('HTTP ' + res.status);
            const logs = await res.json();

            // Build a signature to detect changes (length + last entry time+kind)
            const sig = logs.length + (logs.length > 0 ? logs[logs.length - 1].time + logs[logs.length - 1].kind : '');
            if (sig === lastLogSignature) return; // no change
            lastLogSignature = sig;

            const feedEl = dom.feedContainer;

            // Remove empty message
            const emptyEl = feedEl.querySelector('.feed-empty');
            if (emptyEl) emptyEl.remove();

            if (logs.length <= renderedLogCount) {
                // Logs were trimmed/reset — full redraw
                feedEl.innerHTML = '';
                renderedLogCount = 0;
            }

            // Append only new logs
            const newLogs = logs.slice(renderedLogCount);
            const frag = document.createDocumentFragment();

            newLogs.forEach(log => {
                const el = document.createElement('div');
                el.className = 'log-entry new-entry';
                el.innerHTML =
                    '<span class="log-time">' + escHtml(log.time) + '</span>' +
                    '<span class="log-badge ' + escHtml(log.kind) + '">' + escHtml(log.kind) + '</span>' +
                    '<span class="log-msg">' + escHtml(log.message) + '</span>';
                frag.appendChild(el);

                // Remove animation class after it plays
                setTimeout(() => el.classList.remove('new-entry'), 350);
            });

            feedEl.appendChild(frag);
            renderedLogCount = logs.length;

            // Trim DOM to 200 entries
            while (feedEl.children.length > 200) {
                feedEl.removeChild(feedEl.firstChild);
            }

            // Auto-scroll to bottom unless user scrolled up
            if (!userScrolledUp) {
                feedEl.scrollTop = feedEl.scrollHeight;
            }

        } catch (err) {
            // Silently fail — connection indicator already shows status
        }
    }

    // Simple HTML escaper
    function escHtml(str) {
        if (!str) return '';
        return str.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
    }

    // ── POLLING ──
    setInterval(updateState, 1000);
    setInterval(updateLogs, 1000);
    updateState();
    updateLogs();

})();
