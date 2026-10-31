import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

test("紧凑模型菜单、档位列表、连续高度、首页传递与浅深窄屏", { timeout: 120000 }, async () => {
	const root = join(await testRoot(), "model-picker");
	await mkdir(root, { recursive: true });
	const home = await mkdtemp(join(root, "home-"));
	const model = (id: string, name: string, reasoning: boolean) => ({
		id,
		name,
		reasoning,
		input: ["text"],
		contextWindow: 10000,
		maxTokens: 100,
		...(reasoning
			? {
					thinkingLevelMap: {
						off: null,
						minimal: null,
						low: "low",
						medium: null,
						high: "high",
						xhigh: null,
						max: "max",
					},
				}
			: {}),
	});
	await writeFile(
		join(home, "settings.json"),
		JSON.stringify({
			defaultProvider: "probe",
			defaultModel: "glm",
			defaultTools: ["read"],
			defaultThinkingLevel: "high",
			enabledModels: ["probe/glm", "probe/plain", "probe/long"],
		}),
	);
	await writeFile(
		join(home, "models.json"),
		JSON.stringify({
			providers: {
				probe: {
					api: "openai-completions",
					baseUrl: "http://127.0.0.1:9/v1",
					apiKey: "test-only",
					models: [
						model("glm", "glm-5.3-flash", true),
						model("plain", "无思考模型", false),
						model("long", "用于验证窄屏的模型长名称 Very long model name with extra words", true),
					],
				},
			},
		}),
	);
	const pool = new WebSessionPool({
		cwd: home,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: (load) => load(),
	});
	const port = await freePort(),
		origin = `http://127.0.0.1:${port}`;
	const api = createWebApi(pool, home, port);
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		() => ({ sessions: [], stage: "model-picker" }),
		api,
	);
	await listenWebServer(server, port);
	let browser: BrowserProbe | undefined;
	try {
		const data = await (await fetch(`${origin}/api/models`)).json();
		assert.deepEqual(data.thinkingLevels, ["low", "high", "max"]);
		assert.equal(data.selected.thinkingLevel, "high");
		assert.deepEqual(data.models.find((item: { id: string }) => item.id === "plain").thinkingLevels, ["off"]);
		browser = await BrowserProbe.launch(root);
		const probe = browser;
		await probe.call("Emulation.setEmulatedMedia", {
			features: [{ name: "prefers-reduced-motion", value: "reduce" }],
		});
		await probe.navigate(`${origin}/`);
		await probe.until("document.querySelector('.composer-model')?.textContent.includes('glm-5.3-flash')", "模型就绪");
		await probe.screenshot(join(root, "initial.png"));
		const controlSpacing = await probe.evaluate<{ contextToModel: number; modelToSend: number }>(`(() => {
			const rect = selector => document.querySelector(selector).getBoundingClientRect();
			return {contextToModel:rect('.composer-model').left-rect('.context-meter').right,modelToSend:rect('.composer-send').left-rect('.composer-model').right};
		})()`);
		await probe.click(".composer-model");
		const main = "document.querySelector('.model-picker-dialog')?.dataset.page==='main'";
		async function key(name: string, code: number) {
			for (const type of ["keyDown", "keyUp"])
				await probe.call("Input.dispatchKeyEvent", { type, key: name, code: name, windowsVirtualKeyCode: code });
		}
		async function back(button: string) {
			await key("Escape", 27);
			await probe.until(main, "Esc 返回主菜单");
			await probe.until(`document.activeElement.classList.contains('${button}')`, "返回对应菜单行焦点");
		}
		async function close() {
			await key("Escape", 27);
			await probe.until("!document.querySelector('.model-picker-popover')", "收起选择器");
			await probe.until("document.activeElement.classList.contains('composer-model')", "入口焦点恢复");
		}
		async function strength(level: string, label: string) {
			await probe.click(".model-picker-thinking");
			await probe.click(`[role=option][data-key="${level}"]`);
			await probe.until(main, "选择后返回主菜单");
			await probe.until(
				`document.querySelector('.model-current-strength').textContent===${JSON.stringify(label)}`,
				"强度生效",
			);
		}
		async function hoverFeedback(selector: string) {
			await probe.call("Input.dispatchMouseEvent", { type: "mouseMoved", x: 10, y: 10 });
			const before = await probe.evaluate<string>(
				`getComputedStyle(document.querySelector(${JSON.stringify(selector)})).backgroundColor`,
			);
			const point = await probe.evaluate<{ x: number; y: number }>(`(() => {
				const rect=document.querySelector(${JSON.stringify(selector)}).getBoundingClientRect();
				return {x:rect.left+rect.width/2,y:rect.top+rect.height/2};
			})()`);
			await probe.call("Input.dispatchMouseEvent", { type: "mouseMoved", ...point });
			await probe.until(
				`(() => {const element=document.querySelector(${JSON.stringify(selector)});return element.matches(':hover')&&getComputedStyle(element).backgroundColor!==${JSON.stringify(before)}&&getComputedStyle(element).backgroundColor!=='rgba(0, 0, 0, 0)'})()`,
				"悬停背景反馈",
			);
		}
		async function capture(name: string) {
			await probe.evaluate("document.fonts.ready");
			await probe.screenshot(join(root, name));
		}
		await probe.until(main, "双行主菜单");
		assert.equal(await probe.evaluate("!!document.querySelector('.composer-thinking')"), false);
		await probe.click(".model-picker-thinking");
		assert.deepEqual(
			await probe.evaluate("Array.from(document.querySelectorAll('[role=option]'), e=>e.textContent)"),
			["Low", "High", "Max"],
		);
		await probe.call("Input.dispatchMouseEvent", { type: "mouseMoved", x: 10, y: 10 });
		const beforeHover = await probe.evaluate<string>(
			"getComputedStyle(document.querySelector('[role=option][data-key=low]')).backgroundColor",
		);
		const hoverPoint = await probe.evaluate<{ x: number; y: number }>(`(() => {
			const rect=document.querySelector('[role=option][data-key=low]').getBoundingClientRect();
			return {x:rect.left+rect.width/2,y:rect.top+rect.height/2};
		})()`);
		await probe.call("Input.dispatchMouseEvent", { type: "mouseMoved", ...hoverPoint });
		await probe.until("document.querySelector('[role=option][data-key=low]').matches(':hover')", "悬停未选项");
		const hoveredBackground = await probe.evaluate<string>(
			"getComputedStyle(document.querySelector('[role=option][data-key=low]')).backgroundColor",
		);
		assert.deepEqual(
			{
				hoverBackground: hoveredBackground !== beforeHover && hoveredBackground !== "rgba(0, 0, 0, 0)",
				controlSpacing,
			},
			{ hoverBackground: true, controlSpacing: { contextToModel: 8, modelToSend: 16 } },
			"未选项悬停有背景，入口间距与 X6PhaB 一致",
		);
		await key("End", 35);
		assert.equal(
			await probe.evaluate("document.querySelector('.model-current-strength').textContent"),
			"High",
			"方向键只移动焦点",
		);
		await key("Enter", 13);
		await probe.until(main, "键盘选择返回主菜单");
		assert.equal(await probe.evaluate("document.querySelector('.model-current-strength').textContent"), "Max");
		await close();
		await probe.click(".composer-model");
		assert.equal(
			await probe.evaluate("document.querySelector('.model-current-strength').textContent"),
			"Max",
			"重新打开保留本次选择",
		);
		assert.equal((await pool.list()).length, 0, "首页选择不创建空会话");
		for (const width of [1440, 390]) {
			await close();
			await probe.call("Emulation.setDeviceMetricsOverride", {
				width,
				height: 900,
				deviceScaleFactor: 1,
				mobile: false,
			});
			if (width === 390) await probe.click('.sidebar [aria-label="收起侧栏"]');
			await probe.click(".composer-model");
			for (const theme of ["light", "dark"]) {
				await probe.evaluate(`document.documentElement.dataset.theme='${theme}'`);
				await hoverFeedback(".model-picker-model");
				await capture(`home-panel-${width}-${theme}.png`);
				assert.deepEqual(
					await probe.evaluate(`(() => {
					const p=document.querySelector('.model-picker-popover').getBoundingClientRect();
					return {width:p.width,height:p.height,fits:p.left>=15&&p.right<=innerWidth-15&&p.top>=0};
				})()`),
					{ width: 224, height: 88, fits: true },
				);
				for (const [button, page] of [
					["model-picker-model", "models"],
					["model-picker-thinking", "thinking"],
				]) {
					await probe.click(`.${button}`);
					await probe.until(
						`document.querySelector('.model-picker-dialog')?.dataset.page==='${page}'`,
						"列表视图",
					);
					assert.equal(
						await probe.evaluate(
							"document.querySelector('.model-picker-popover').getBoundingClientRect().height",
						),
						120,
					);
					assert.equal(
						await probe.evaluate("document.querySelectorAll('.model-picker-list [role=option]').length"),
						3,
					);
					assert.equal(
						await probe.evaluate(
							"document.querySelector('.model-picker-list [role=option]').getBoundingClientRect().height",
						),
						32,
					);
					assert.equal(
						await probe.evaluate("!!document.querySelector('.model-picker-list [role=option] svg')"),
						false,
						"选中项不占用图标空间",
					);
					assert.notEqual(
						await probe.evaluate(
							"getComputedStyle(document.querySelector('[role=option][aria-selected=true]')).backgroundColor",
						),
						"rgba(0, 0, 0, 0)",
					);
					assert.equal(
						await probe.evaluate(
							"document.querySelector('[data-page-id=\"1\"]').inert && !document.querySelector('[data-page-id=\"2\"]').inert",
						),
						true,
						"仅当前页可操作",
					);
					assert.equal(
						await probe.evaluate("document.querySelectorAll('.t-page-slide .t-page')[1].getAnimations().length"),
						0,
						"减少动态时无页面滑动",
					);
					const selectedBackground = await probe.evaluate<string>(
						"getComputedStyle(document.querySelector('[role=option][aria-selected=true]')).backgroundColor",
					);
					await hoverFeedback(".model-picker-list [role=option][aria-selected=false]");
					assert.equal(
						await probe.evaluate(
							"getComputedStyle(document.querySelector('[role=option][aria-selected=true]')).backgroundColor",
						),
						selectedBackground,
						"悬停其他项时保留选中背景",
					);
					await capture(`home-${page}-${width}-${theme}.png`);
					await back(button);
				}
				await probe.call("Emulation.setEmulatedMedia", {
					features: [{ name: "prefers-reduced-motion", value: "no-preference" }],
				});
				await probe.evaluate("document.querySelector('.model-picker-model').click()");
				await probe.until(
					"document.querySelector('.model-picker-pages').getAnimations({subtree:true}).length>0",
					"左右滑动开始",
				);
				await probe.evaluate(
					`document.querySelector('.model-picker-popover').getAnimations({subtree:true}).forEach(animation=>{animation.pause();animation.currentTime=80})`,
				);
				await capture(`slide-mid-${width}-${theme}.png`);
				await probe.evaluate(
					"document.querySelector('.model-picker-popover').getAnimations({subtree:true}).forEach(animation=>animation.finish())",
				);
				await probe.call("Emulation.setEmulatedMedia", {
					features: [{ name: "prefers-reduced-motion", value: "reduce" }],
				});
				await back("model-picker-model");
			}
		}
		// 正常动态下逐帧检查高度插值、右下锚点和中途返回。
		await probe.call("Emulation.setEmulatedMedia", {
			features: [{ name: "prefers-reduced-motion", value: "no-preference" }],
		});
		const frames = await probe.evaluate<
			Array<{
				height: number;
				bottom: number;
				right: number;
				mainX: number;
				listX: number;
				mainOpacity: number;
				listOpacity: number;
				mainFilter: string;
				listFilter: string;
			}>
		>(`new Promise((resolve, reject) => {
			const frames=[];
			setTimeout(() => reject(new Error("高度采样超时："+document.body.innerText.slice(-500))), 2000);
			document.querySelector('.model-picker-thinking').click();
			const start=performance.now();
			function sample() {
				const p=document.querySelector('.model-picker-popover').getBoundingClientRect();
				const main=getComputedStyle(document.querySelector('[data-page-id="1"]')), list=getComputedStyle(document.querySelector('[data-page-id="2"]'));
				frames.push({height:p.height,bottom:p.bottom,right:p.right,mainX:new DOMMatrix(main.transform).m41,listX:new DOMMatrix(list.transform).m41,mainOpacity:Number(main.opacity),listOpacity:Number(list.opacity),mainFilter:main.filter,listFilter:list.filter});
				if(performance.now()-start<450) requestAnimationFrame(sample); else resolve(frames);
			}
			requestAnimationFrame(sample);
		})`);
		assert.ok(
			frames.some((frame) => frame.height > 88.5 && frame.height < 119.5),
			"高度经过中间值",
		);
		assert.ok(Math.abs(frames.at(-1)!.height - 120) < 1, "列表结束高度");
		assert.ok(
			frames.some((frame) => frame.mainX < -0.1 && frame.listX > 0.1 && frame.height > 88.5 && frame.height < 119.5),
			"左右滑动与高度同步",
		);
		assert.ok(
			frames.some(
				(frame) => frame.mainOpacity > 0 && frame.mainOpacity < 1 && frame.listOpacity > 0 && frame.listOpacity < 1,
			),
			"两页交叠淡入淡出",
		);
		assert.equal(frames.at(-1)!.listX, 0, "列表停止在原位");
		assert.ok(
			frames.every((frame) => frame.mainFilter === "none" && frame.listFilter === "none"),
			"切换全过程无模糊",
		);
		assert.ok(
			Math.max(...frames.map((frame) => frame.bottom)) - Math.min(...frames.map((frame) => frame.bottom)) < 3,
			"底边锚点连续",
		);
		assert.ok(
			Math.max(...frames.map((frame) => frame.right)) - Math.min(...frames.map((frame) => frame.right)) < 1,
			"固定宽度右对齐",
		);
		const returning = await probe.evaluate<
			Array<{ mainX: number; listX: number; height: number }>
		>(`new Promise((resolve, reject) => {
			const frames=[]; const start=performance.now();
			setTimeout(() => reject(new Error("返回滑动采样超时")),2000);
			document.activeElement.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true,cancelable:true}));
			function sample() {
				const main=getComputedStyle(document.querySelector('[data-page-id="1"]')), list=getComputedStyle(document.querySelector('[data-page-id="2"]'));
				frames.push({mainX:new DOMMatrix(main.transform).m41,listX:new DOMMatrix(list.transform).m41,height:document.querySelector('.model-picker-popover').getBoundingClientRect().height});
				if(performance.now()-start<450) requestAnimationFrame(sample); else resolve(frames);
			}
			requestAnimationFrame(sample);
		})`);
		assert.ok(
			returning.some(
				(frame) => frame.mainX < -0.1 && frame.listX > 0.1 && frame.height > 88.5 && frame.height < 119.5,
			),
			"返回时反向滑动并收缩高度",
		);
		assert.equal(returning.at(-1)!.mainX, 0, "主菜单停止在原位");
		await probe.until("document.activeElement.classList.contains('model-picker-thinking')", "返回行焦点");
		await probe.until(
			"Math.abs(document.querySelector('.model-picker-popover').getBoundingClientRect().height-88)<0.1",
			"收回主菜单高度",
		);
		const reversal = await probe.evaluate<number[]>(`new Promise((resolve, reject) => {
			const heights=[];
			setTimeout(() => reject(new Error("反向采样超时")), 2000);
			document.querySelector('.model-picker-model').click();
			const start=performance.now(); let returned=false;
			function sample() {
				heights.push(document.querySelector('.model-picker-popover').getBoundingClientRect().height);
				if (!returned && performance.now()-start>70) {
					returned=true;
					document.activeElement.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true,cancelable:true}));
				}
				if(performance.now()-start<500) requestAnimationFrame(sample); else resolve(heights);
			}
			requestAnimationFrame(sample);
		})`);
		assert.ok(Math.max(...reversal) > 89 && Math.max(...reversal) < 120, "展开中途返回");
		assert.ok(
			reversal.slice(1).every((height, index) => Math.abs(height - reversal[index]) < 20),
			"反向无高度跳变",
		);
		await probe.call("Emulation.setEmulatedMedia", {
			features: [{ name: "prefers-reduced-motion", value: "reduce" }],
		});
		await close();
		await probe.fill(".composer-input", "验证本次新会话的强度");
		await probe.click(".composer-send");
		await probe.until("location.pathname.startsWith('/s/') && !!document.querySelector('.chat-loaded')", "新建会话");
		const id = await probe.evaluate<string>("location.pathname.split('/')[2]");
		const slot = await pool.openSaved(id);
		assert.equal(slot.session.thinkingLevel, "max", "首页强度传入新会话");
		assert.deepEqual(data.thinkingLevels, slot.session.getAvailableThinkingLevels(), "首页与会话支持档位一致");
		await probe.click(".composer-model");
		await strength("high", "High");
		assert.equal(slot.session.thinkingLevel, "high");
		await strength("low", "Low");
		await probe.evaluate(`(() => {
			const original=window.fetch;
			window.fetch=(input,init)=>String(input).endsWith('/api/models/thinking') ? Promise.resolve(new Response(JSON.stringify({error:{message:'测试保存失败',code:'test'}}),{status:500,headers:{'Content-Type':'application/json'}})) : original(input,init);
			window.restoreFetch=()=>{window.fetch=original};
		})()`);
		await probe.click(".model-picker-thinking");
		await probe.click('[role=option][data-key="max"]');
		await probe.until(
			"document.querySelector('.model-picker-dialog').dataset.busy==='false' && document.body.textContent.includes('测试保存失败')",
			"保存失败",
		);
		assert.equal(
			await probe.evaluate("document.querySelector('.model-picker-dialog').dataset.page"),
			"thinking",
			"失败保留列表",
		);
		assert.equal(
			await probe.evaluate("document.querySelector('[role=option][aria-selected=true]').dataset.key"),
			"low",
			"失败保留已确认值",
		);
		assert.equal(slot.session.thinkingLevel, "low");
		await probe.until("!!document.activeElement.closest('[role=listbox]')", "失败后焦点保留在列表");
		await probe.evaluate("window.restoreFetch()");
		await probe.click('[role=option][data-key="max"]');
		await probe.until(main, "重试返回主菜单");
		assert.equal(slot.session.thinkingLevel, "max");
		await probe.click(".model-picker-model");
		await probe.click('[role=option][data-key="probe/plain"]');
		await probe.until(main, "选模型返回主菜单");
		await probe.click(".model-picker-thinking");
		assert.deepEqual(
			await probe.evaluate("Array.from(document.querySelectorAll('[role=option]'), e=>e.textContent)"),
			["Off"],
			"无思考模型只有 Off",
		);
		assert.equal(
			await probe.evaluate("document.querySelector('.model-picker-popover').getBoundingClientRect().height"),
			56,
		);
		await probe.click('[role=option][data-key="off"]');
		await probe.until(main, "单击当前档位返回主菜单");
		await probe.click(".model-picker-model");
		await probe.evaluate("document.querySelector('.model-picker-list [role=listbox]').focus()");
		await key("End", 35);
		assert.equal(
			await probe.evaluate("!!document.querySelector('.model-picker-list')"),
			true,
			"方向键只移动模型焦点",
		);
		await key("Enter", 13);
		await probe.until(
			`document.querySelector('.composer-model').textContent.includes('Very long') && ${main}`,
			"切换长名称模型",
		);
		for (const theme of ["light", "dark"]) {
			await probe.evaluate(`document.documentElement.dataset.theme='${theme}'`);
			await capture(`chat-390-${theme}.png`);
			assert.equal(await probe.evaluate("document.documentElement.scrollWidth<=innerWidth"), true);
		}
		await close();
		await probe.click(".context-meter");
		await probe.until(
			"document.querySelector('.context-meter').getAttribute('aria-expanded')==='true'",
			"上下文信息保留",
		);
		assert.equal(
			await probe.evaluate("document.querySelector('.context-panel').textContent.includes('上下文容量')"),
			true,
		);
		await key("Escape", 27);
		await probe.click(".composer-model");
		await probe.click(".model-picker-model");
		await probe.evaluate(
			"document.querySelector('.model-picker-list [role=listbox]').dispatchEvent(new Event('scroll'))",
		);
		assert.equal(await probe.evaluate("!!document.querySelector('.model-picker-list')"), true, "内部滚动保留列表");
		await probe.evaluate("document.querySelector('.conversation').dispatchEvent(new Event('scroll'))");
		await probe.until("!document.querySelector('.model-picker-popover')", "页面滚动关闭");
		for (const button of [null, ".model-picker-model", ".model-picker-thinking"]) {
			await probe.click(".composer-model");
			if (button) await probe.click(button);
			for (const type of ["mousePressed", "mouseReleased"])
				await probe.call("Input.dispatchMouseEvent", { type, button: "left", clickCount: 1, x: 380, y: 40 });
			await probe.until("!document.querySelector('.model-picker-popover')", "任意层点击外部直接关闭");
		}
		await probe.evaluate(`(() => {
			const original=window.fetch;
			window.pickerWrites=0;
			window.fetch=(input,init)=>String(input).endsWith('/api/models/select') ? (window.pickerWrites++, new Promise(resolve => {
				window.releasePicker=()=>resolve(original(input,init));
			})) : original(input,init);
			window.restoreFetch=()=>{window.fetch=original};
		})()`);
		await probe.click(".composer-model");
		await probe.click(".model-picker-model");
		await probe.click('[role=option][data-key="probe/glm"]');
		await probe.until("document.querySelector('.model-picker-dialog').dataset.busy==='true'", "保存中");
		assert.equal(
			await probe.evaluate(
				"Array.from(document.querySelectorAll('[role=option]')).every(e=>e.getAttribute('aria-disabled')==='true')",
			),
			true,
			"保存中禁用重复选择",
		);
		await probe.evaluate("document.querySelector('[role=option][data-key=\"probe/plain\"]').click()");
		assert.equal(await probe.evaluate("window.pickerWrites"), 1);
		await back("model-picker-model");
		await close();
		await probe.click(".composer-model");
		await probe.click(".model-picker-thinking");
		await probe.evaluate("window.releasePicker()");
		await probe.until("document.querySelector('.model-picker-dialog').dataset.busy==='false'", "等待保存结束");
		assert.equal(
			await probe.evaluate("document.querySelector('.model-picker-dialog').dataset.page"),
			"thinking",
			"过期响应保持当前视图",
		);
		await probe.evaluate("window.restoreFetch()");
		await back("model-picker-thinking");
		await close();
		// 为超高菜单提供隔离的读取响应，验证实际内部滚动与末项键盘可达。
		await probe.evaluate(`(() => {
			const original=window.fetch;
			window.fetch=async(input,init)=>{
				const response=await original(input,init);
				if(String(input).includes('/api/models') && init?.method==='GET') {
					const data=await response.json();
					data.models.push(...Array.from({length:20},(_,i)=>({...data.models.find(m=>m.provider==='probe'),id:'extra-'+i,name:'滚动验证模型 '+i})));
					data.enabled=data.models.map(m=>m.provider+'/'+m.id);
					return new Response(JSON.stringify(data),{headers:{'Content-Type':'application/json'}});
				}
				return response;
			};
			window.restoreFetch=()=>{window.fetch=original};
		})()`);
		await probe.click(".composer-model");
		await probe.click(".model-picker-model");
		await probe.until("document.querySelectorAll('[role=option]').length>20", "超高模型清单");
		assert.equal(
			await probe.evaluate("document.querySelector('.model-picker-popover').getBoundingClientRect().height"),
			440,
			"菜单限制最大高度",
		);
		await key("End", 35);
		await probe.until("document.querySelector('[role=listbox]').scrollTop>0", "键盘滚动至末项");
		assert.equal(await probe.evaluate("document.activeElement.dataset.key"), "probe/extra-19");
		await probe.evaluate("window.restoreFetch()");
		await back("model-picker-model");
		await close();
		const settings = JSON.parse(await readFile(join(home, "settings.json"), "utf8"));
		assert.equal(settings.defaultModel, "glm");
		assert.equal(settings.defaultThinkingLevel, "high", "本次选择不更改默认设置");
	} finally {
		await browser?.close();
		await api.close();
		await pool.close();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
		await rm(home, { recursive: true, force: true });
	}
});
