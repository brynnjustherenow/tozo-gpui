
(function() {
    if (!navigator.mediaSession) {
        return;
    }

    const originalDesc = Object.getOwnPropertyDescriptor(
        Object.getPrototypeOf(navigator.mediaSession),
        'metadata'
    );
    if (!originalDesc || !originalDesc.set || !originalDesc.get) {
        return;
    }

    const MARK = '\u2060';

    const pathOf = () => window.location.origin + window.location.pathname;

    const fallbackTitle = () => {
        const t = document.title || '';
        const cut = t.split('_哔哩哔哩')[0].trim();
        return cut || t;
    };

    const enrich = (meta) => {
        const href = pathOf();
        const baseTitle = meta.title || fallbackTitle();
        const title = baseTitle
            ? `${baseTitle}${MARK} [via ${href}]`
            : `${MARK}无标题 [via ${href}]`;
        return new MediaMetadata({
            title,
            artist: meta.artist || `${MARK}${href}`,
            album: meta.album || '',
            artwork: Array.isArray(meta.artwork) ? meta.artwork : [],
        });
    };

    const resend = (href) => {
        const current = originalDesc.get.call(navigator.mediaSession);
        if (!current) {
            return;
        }
        const title = fallbackTitle();
        originalDesc.set.call(navigator.mediaSession, new MediaMetadata({
            title: title ? `${title}${MARK} [via ${href}]` : `${MARK}无标题 [via ${href}]`,
            artist: `${MARK}${href}`,
            album: '',
            artwork: Array.isArray(current.artwork) ? current.artwork : [],
        }));
    };

    const existingMeta = originalDesc.get.call(navigator.mediaSession);
    if (existingMeta) {
        try {
            originalDesc.set.call(navigator.mediaSession, enrich(existingMeta));
        } catch (e) {
            console.warn('metadata enrich failed', e);
        }
    }

    Object.defineProperty(navigator.mediaSession, 'metadata', {
        configurable: true,
        enumerable: true,
        get: function() {
            return originalDesc.get.call(navigator.mediaSession);
        },
        set: function(newMetadata) {
            console.debug(`[media-session] set from ${pathOf()}`);
            if (newMetadata && typeof newMetadata === 'object') {
                try {
                    newMetadata = enrich(newMetadata);
                } catch (e) {
                    console.warn('metadata enrich failed', e);
                }
            }
            originalDesc.set.call(navigator.mediaSession, newMetadata);
        },
    });

    let lastPath = pathOf();
    setInterval(() => {
        const href = pathOf();
        if (href === lastPath) {
            return;
        }
        lastPath = href;
        setTimeout(() => resend(href), 1500);
    }, 1000);

    console.log('Media Session 拦截器已启动，额外信息已注入！');
})();
